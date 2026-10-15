//! Parts of the frame: what of a layer still shows once the opaque layers
//! over it are drawn, and the rectangles of pixels a pass is limited to, so
//! a layer mostly covered costs only what shows of it.
//!
//! Positions are the render passes' own UV: (0, 0) is a target's first
//! pixel, x runs across and y down, and 1 is the far edge, whatever the
//! target's size. A scissor rectangle counts pixels the same way.

/// A rectangle of the frame in UV. Empty when it has no width or height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Area {
    /// The whole frame.
    pub const FRAME: Self = Self {
        x0: 0.0,
        y0: 0.0,
        x1: 1.0,
        y1: 1.0,
    };

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// The part of this area inside `other`, perhaps empty.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    /// This area with `dx` and `dy` taken off every side (added back with
    /// negative values).
    #[must_use]
    pub fn shrunk(&self, dx: f32, dy: f32) -> Self {
        Self {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 - dx,
            y1: self.y1 - dy,
        }
    }

    /// The smallest area holding every point given.
    #[must_use]
    pub fn around(points: impl IntoIterator<Item = [f32; 2]>) -> Self {
        let mut area = Self {
            x0: f32::INFINITY,
            y0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            y1: f32::NEG_INFINITY,
        };
        for [x, y] in points {
            area.x0 = area.x0.min(x);
            area.y0 = area.y0.min(y);
            area.x1 = area.x1.max(x);
            area.y1 = area.y1.max(y);
        }
        area
    }

    pub fn corners(&self) -> [[f32; 2]; 4] {
        [
            [self.x0, self.y0],
            [self.x1, self.y0],
            [self.x0, self.y1],
            [self.x1, self.y1],
        ]
    }

    /// The pixels of a `width` × `height` target lying wholly inside this
    /// area, less `margin` pixels on every side: where a picture filling it
    /// is certain to be opaque, whatever its edges blend with.
    #[must_use]
    pub fn inner_pixels(&self, width: u32, height: u32, margin: u32) -> Option<Pixels> {
        let clip = |lo: f32, hi: f32, size: u32| {
            let size_f = size as f32;
            let lo = ((lo * size_f).ceil().max(0.0) as u32).saturating_add(margin);
            let hi = ((hi * size_f).floor().clamp(0.0, size_f) as u32).saturating_sub(margin);
            (lo < hi).then_some((lo, hi.min(size)))
        };
        let (x0, x1) = clip(self.x0, self.x1, width)?;
        let (y0, y1) = clip(self.y0, self.y1, height)?;
        Some(Pixels { x0, y0, x1, y1 })
    }

    /// Every pixel of a `width` × `height` target this area touches, and
    /// `margin` more on every side, within the target: what a pass has to
    /// draw for the area to come out right when read back with filtering.
    #[must_use]
    pub fn outer_pixels(&self, width: u32, height: u32, margin: u32) -> Option<Pixels> {
        let clip = |lo: f32, hi: f32, size: u32| {
            let size_f = size as f32;
            let lo = (lo * size_f).floor().clamp(0.0, size_f) as u32;
            let hi = (hi * size_f).ceil().clamp(0.0, size_f) as u32;
            // Off the target altogether, it needs nothing drawn round it.
            (lo < hi).then(|| {
                (
                    lo.saturating_sub(margin),
                    hi.saturating_add(margin).min(size),
                )
            })
        };
        let (x0, x1) = clip(self.x0, self.x1, width)?;
        let (y0, y1) = clip(self.y0, self.y1, height)?;
        Some(Pixels { x0, y0, x1, y1 })
    }
}

/// A rectangle of pixels: columns `x0..x1`, rows `y0..y1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pixels {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Pixels {
    /// All of a `width` × `height` target.
    #[must_use]
    pub fn all(width: u32, height: u32) -> Self {
        Self {
            x0: 0,
            y0: 0,
            x1: width,
            y1: height,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// How many pixels it holds.
    pub fn count(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            u64::from(self.x1 - self.x0) * u64::from(self.y1 - self.y0)
        }
    }

    /// The same rectangle in UV of a `width` × `height` target.
    #[must_use]
    pub fn area(&self, width: u32, height: u32) -> Area {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        Area {
            x0: self.x0 as f32 / w,
            y0: self.y0 as f32 / h,
            x1: self.x1 as f32 / w,
            y1: self.y1 as f32 / h,
        }
    }

    pub(crate) fn intersect(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }

    /// The parts of this rectangle outside `hole`: none, itself, or up to
    /// four pieces — the full-width bands above and below the hole, and the
    /// pieces left and right of it between them.
    fn minus(&self, hole: &Self) -> Vec<Self> {
        let cut = self.intersect(hole);
        if cut.is_empty() {
            return vec![*self];
        }
        let pieces = [
            Self {
                y1: cut.y0,
                ..*self
            },
            Self {
                y0: cut.y1,
                ..*self
            },
            Self {
                y0: cut.y0,
                y1: cut.y1,
                x1: cut.x0,
                ..*self
            },
            Self {
                y0: cut.y0,
                y1: cut.y1,
                x0: cut.x1,
                ..*self
            },
        ];
        pieces.into_iter().filter(|p| !p.is_empty()).collect()
    }
}

/// `parts` (rectangles that do not overlap) less `hole`, still not
/// overlapping.
pub fn subtract(parts: &[Pixels], hole: &Pixels) -> Vec<Pixels> {
    parts.iter().flat_map(|part| part.minus(hole)).collect()
}

/// The pixels of all the rectangles given, as rectangles that do not
/// overlap — so a pass drawing through each in turn touches every pixel
/// once, which matters where it blends.
pub fn disjoint(rects: impl IntoIterator<Item = Pixels>) -> Vec<Pixels> {
    let mut out: Vec<Pixels> = Vec::new();
    for rect in rects {
        let mut pieces = vec![rect];
        for placed in &out {
            pieces = subtract(&pieces, placed);
        }
        out.extend(pieces.into_iter().filter(|p| !p.is_empty()));
    }
    out
}

/// More rectangles than this and a pass is not worth splitting: it draws
/// whole instead.
pub const MAX_PARTS: usize = 12;

/// The scissor rectangles a pass on a `width` × `height` target draws
/// through to get `areas` right: `None` when that is the whole target or
/// too many pieces. `margin` pixels more are drawn round each area, for a
/// later pass reading it with filtering.
pub fn scissors_for(areas: &[Area], width: u32, height: u32, margin: u32) -> Option<Vec<Pixels>> {
    let rects = disjoint(
        areas
            .iter()
            .filter_map(|area| area.outer_pixels(width, height, margin)),
    );
    let all = Pixels::all(width, height);
    let whole = rects.iter().map(Pixels::count).sum::<u64>() >= all.count();
    (rects.len() <= MAX_PARTS && !whole).then_some(rects)
}

/// Draw the full-frame triangle of the pipeline and bind group already set,
/// through each scissor rectangle in turn — or once, everywhere, for `None`.
pub fn draw_within(pass: &mut wgpu::RenderPass<'_>, within: Option<&[Pixels]>) {
    match within {
        None => pass.draw(0..3, 0..1),
        Some(rects) => {
            for r in rects {
                pass.set_scissor_rect(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(x0: u32, y0: u32, x1: u32, y1: u32) -> Pixels {
        Pixels { x0, y0, x1, y1 }
    }

    fn total(rects: &[Pixels]) -> u64 {
        rects.iter().map(Pixels::count).sum()
    }

    fn overlaps(rects: &[Pixels]) -> bool {
        rects
            .iter()
            .enumerate()
            .any(|(i, a)| rects[i + 1..].iter().any(|b| !a.intersect(b).is_empty()))
    }

    #[test]
    fn a_hole_in_the_middle_leaves_four_strips() {
        let frame = px(0, 0, 100, 50);
        let left = subtract(&[frame], &px(10, 5, 90, 45));
        assert_eq!(left.len(), 4);
        assert_eq!(total(&left), 100 * 50 - 80 * 40);
        assert!(!overlaps(&left));
    }

    #[test]
    fn a_hole_at_the_edge_leaves_one_strip() {
        let left = subtract(&[px(0, 0, 100, 50)], &px(0, 0, 80, 50));
        assert_eq!(left, vec![px(80, 0, 100, 50)]);
    }

    #[test]
    fn a_hole_over_everything_leaves_nothing() {
        assert!(subtract(&[px(0, 0, 100, 50)], &px(0, 0, 100, 50)).is_empty());
        assert!(subtract(&[px(10, 10, 20, 20)], &px(0, 0, 100, 50)).is_empty());
    }

    #[test]
    fn a_hole_elsewhere_leaves_it_whole() {
        let part = px(0, 0, 10, 10);
        assert_eq!(subtract(&[part], &px(20, 20, 30, 30)), vec![part]);
    }

    #[test]
    fn overlapping_rectangles_come_apart_without_losing_a_pixel() {
        let rects = disjoint([px(0, 0, 10, 10), px(5, 5, 15, 15), px(0, 0, 3, 3)]);
        assert!(!overlaps(&rects));
        assert_eq!(total(&rects), 100 + 100 - 25);
    }

    #[test]
    fn inner_pixels_are_the_ones_wholly_inside() {
        let area = Area {
            x0: 0.105,
            y0: 0.1,
            x1: 0.5,
            y1: 0.899,
        };
        // 10.5 → 11 in, 50 out; 10 → 10, 89.9 → 89.
        assert_eq!(area.inner_pixels(100, 100, 0), Some(px(11, 10, 50, 89)));
        assert_eq!(area.inner_pixels(100, 100, 2), Some(px(13, 12, 48, 87)));
        assert_eq!(area.inner_pixels(100, 100, 40), None);
    }

    #[test]
    fn outer_pixels_are_every_one_touched_and_stay_on_the_target() {
        let area = Area {
            x0: 0.105,
            y0: -0.2,
            x1: 0.5,
            y1: 0.899,
        };
        assert_eq!(area.outer_pixels(100, 100, 0), Some(px(10, 0, 50, 90)));
        assert_eq!(area.outer_pixels(100, 100, 1), Some(px(9, 0, 51, 91)));
        let off = Area {
            x0: 1.2,
            y0: 0.0,
            x1: 1.5,
            y1: 1.0,
        };
        assert_eq!(off.outer_pixels(100, 100, 1), None);
    }

    #[test]
    fn scissors_are_left_off_when_they_would_cover_it_all_or_split_it_too_far() {
        let strip = Area {
            x0: 0.0,
            y0: 0.0,
            x1: 0.1,
            y1: 1.0,
        };
        assert_eq!(
            scissors_for(&[strip], 100, 50, 1),
            Some(vec![px(0, 0, 11, 50)])
        );
        assert_eq!(scissors_for(&[Area::FRAME], 100, 50, 0), None);
        let many: Vec<Area> = (0..20)
            .map(|i| Area {
                x0: i as f32 * 0.05,
                y0: 0.0,
                x1: i as f32 * 0.05 + 0.01,
                y1: 0.1,
            })
            .collect();
        assert_eq!(scissors_for(&many, 1000, 1000, 0), None);
    }
}
