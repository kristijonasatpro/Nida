//! A mask on an effect: the effect shows only inside a shape — a rectangle,
//! an ellipse, or an outline drawn by hand — its edge feathered, or only
//! outside the shape. A blur over a face, a colour change on one corner.
//!
//! Positions are in UV of the frame: (0, 0) its top left, (1, 1) its bottom
//! right.

use serde::{Deserialize, Serialize};

/// The effect that carries a layer's own mask (Input Settings → **Mask**):
/// it leaves the layer transparent, so masked it cuts a hole in the layer —
/// or, inverted, keeps only the shape. Kept last in the layer's chain, so
/// the hole goes through everything the effects before it drew.
pub const LAYER_MASK_EFFECT: &str = "cutout";

/// The most points a drawn outline keeps; a longer one is simplified.
pub const MAX_PATH_POINTS: usize = 64;

/// The most feather, in 1080ths of the frame's height.
pub const MAX_FEATHER: f32 = 500.0;

/// Where a mask is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaskShape {
    /// A rectangle, by its left, top, width and height.
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    /// The ellipse inside that rectangle — a circle, when it is square on
    /// the screen.
    Ellipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    /// An outline drawn by hand, closed back to its first point.
    Path { points: Vec<[f32; 2]> },
}

/// A mask: where, how soft its edge, and which side of it the effect is on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EffectMask {
    pub shape: MaskShape,
    /// How soft the edge is: the width of the fade across it, in pixels of a
    /// 1080-line picture (whatever the size), half inside the shape and half
    /// outside. 0 is a clean edge.
    #[serde(default)]
    pub feather: f32,
    /// The effect outside the shape, and the picture as it was inside.
    #[serde(default)]
    pub invert: bool,
}

impl EffectMask {
    /// This mask fit to draw: numbers finite, feather within 0 to
    /// [`MAX_FEATHER`], a rectangle or ellipse the right way round, an
    /// outline of at most [`MAX_PATH_POINTS`]. `None` when nothing is left
    /// of it — no area, or an outline of fewer than three points.
    #[must_use]
    pub fn sanitized(self) -> Option<Self> {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let feather = finite(self.feather).clamp(0.0, MAX_FEATHER);
        let area = |x: f32, y: f32, width: f32, height: f32| {
            let (x, y, width, height) = (finite(x), finite(y), finite(width), finite(height));
            // Drawn right to left or bottom to top: the same rectangle.
            let (x, width) = if width < 0.0 {
                (x + width, -width)
            } else {
                (x, width)
            };
            let (y, height) = if height < 0.0 {
                (y + height, -height)
            } else {
                (y, height)
            };
            (width > 0.0 && height > 0.0).then_some((x, y, width, height))
        };
        let shape = match self.shape {
            MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            } => {
                let (x, y, width, height) = area(x, y, width, height)?;
                MaskShape::Rectangle {
                    x,
                    y,
                    width,
                    height,
                }
            }
            MaskShape::Ellipse {
                x,
                y,
                width,
                height,
            } => {
                let (x, y, width, height) = area(x, y, width, height)?;
                MaskShape::Ellipse {
                    x,
                    y,
                    width,
                    height,
                }
            }
            MaskShape::Path { points } => {
                let points: Vec<[f32; 2]> = points
                    .into_iter()
                    .filter(|p| p[0].is_finite() && p[1].is_finite())
                    .collect();
                let points = simplify(&points, MAX_PATH_POINTS);
                if points.len() < 3 {
                    return None;
                }
                MaskShape::Path { points }
            }
        };
        Some(Self {
            shape,
            feather,
            invert: self.invert,
        })
    }
}

/// An outline cut to at most `max` points, keeping its shape: the points
/// that matter least — those nearest the line between their neighbours —
/// go first (Ramer–Douglas–Peucker, the tolerance raised until few enough
/// are left).
pub fn simplify(points: &[[f32; 2]], max: usize) -> Vec<[f32; 2]> {
    if points.len() <= max {
        return points.to_vec();
    }
    let mut tolerance = 1e-4_f32;
    loop {
        let kept = douglas_peucker(points, tolerance);
        if kept.len() <= max {
            return kept;
        }
        tolerance *= 1.5;
    }
}

fn douglas_peucker(points: &[[f32; 2]], tolerance: f32) -> Vec<[f32; 2]> {
    fn distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-9 {
            return ((p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)).sqrt();
        }
        ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / len
    }
    fn keep(points: &[[f32; 2]], tolerance: f32, out: &mut Vec<[f32; 2]>) {
        let (first, last) = (points[0], points[points.len() - 1]);
        let far = points[1..points.len() - 1]
            .iter()
            .enumerate()
            .map(|(i, &p)| (i + 1, distance(p, first, last)))
            .fold((0, 0.0_f32), |a, b| if b.1 > a.1 { b } else { a });
        if far.1 > tolerance {
            keep(&points[..=far.0], tolerance, out);
            out.pop();
            keep(&points[far.0..], tolerance, out);
        } else {
            out.push(first);
            out.push(last);
        }
    }
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = Vec::new();
    keep(points, tolerance, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_drawn_backwards_is_the_same_rectangle() {
        let mask = EffectMask {
            shape: MaskShape::Rectangle {
                x: 0.6,
                y: 0.5,
                width: -0.2,
                height: -0.3,
            },
            feather: 20.0,
            invert: false,
        };
        match mask.sanitized().unwrap().shape {
            MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            } => {
                assert!((x - 0.4).abs() < 1e-6 && (y - 0.2).abs() < 1e-6);
                assert!((width - 0.2).abs() < 1e-6 && (height - 0.3).abs() < 1e-6);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_is_left_of_a_mask_without_area() {
        let flat = EffectMask {
            shape: MaskShape::Ellipse {
                x: 0.1,
                y: 0.1,
                width: 0.0,
                height: 0.5,
            },
            feather: 0.0,
            invert: false,
        };
        assert!(flat.sanitized().is_none());
        let line = EffectMask {
            shape: MaskShape::Path {
                points: vec![[0.1, 0.1], [0.5, 0.5]],
            },
            feather: 0.0,
            invert: false,
        };
        assert!(line.sanitized().is_none());
    }

    #[test]
    fn feather_is_kept_within_bounds() {
        let mask = EffectMask {
            shape: MaskShape::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            feather: f32::INFINITY,
            invert: true,
        };
        let mask = mask.sanitized().unwrap();
        assert!(mask.feather.abs() < f32::EPSILON, "not finite: none");
        assert!(mask.invert);
    }

    #[test]
    fn a_long_drawn_outline_is_simplified_keeping_its_corners() {
        // A square drawn with 400 points along its sides.
        let mut points = Vec::new();
        for side in 0..4 {
            for i in 0..100 {
                let t = i as f32 / 100.0;
                points.push(match side {
                    0 => [0.2 + 0.6 * t, 0.2],
                    1 => [0.8, 0.2 + 0.6 * t],
                    2 => [0.8 - 0.6 * t, 0.8],
                    _ => [0.2, 0.8 - 0.6 * t],
                });
            }
        }
        let kept = simplify(&points, MAX_PATH_POINTS);
        assert!(
            kept.len() <= MAX_PATH_POINTS && kept.len() >= 4,
            "{}",
            kept.len()
        );
        for corner in [[0.8, 0.2], [0.8, 0.8], [0.2, 0.8]] {
            assert!(
                kept.iter()
                    .any(|p| (p[0] - corner[0]).abs() < 0.01 && (p[1] - corner[1]).abs() < 0.01),
                "corner {corner:?} kept: {kept:?}"
            );
        }
    }
}
