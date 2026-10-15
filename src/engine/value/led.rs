//! LED screens on one feed, as Resolume's Advanced Output does it.
//!
//! An event's LED is rarely one 16:9 screen: a 5×3 m wall and two 1×1 m
//! cubes, say, all driven by one LED processor. The processor takes one HDMI
//! signal — the *feed* — and shows a fixed pixel rectangle of it on each
//! screen. So Varda sends one feed with every screen's picture placed at the
//! exact pixels the processor reads it from.
//!
//! Each screen shows Main or a bus, either whole (fitted, filled or
//! stretched to the screen) or as its part of that picture: screens that
//! take parts of one source continue one image across them, the way they
//! stand in the room.

use super::render::OutputSource;

/// The most screens one feed carries.
pub const MAX_LED_SCREENS: usize = 32;
/// The largest size, in pixels, of a feed or a screen.
pub const MAX_LED_PIXELS: u32 = 16_384;

/// The screens one output's feed carries.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct LedLayout {
    /// The feed's size in pixels: what the LED processor takes in, such as
    /// 1920×1080 or 3840×2160.
    pub feed: [u32; 2],
    /// The screens, drawn in order (a later one covers an earlier one).
    #[serde(default)]
    pub screens: Vec<LedScreen>,
    /// Every screen shows a test card — its name, size, border and a grid —
    /// instead of its picture, to line the feed up with the processor.
    #[serde(default)]
    pub test_card: bool,
}

/// One LED screen: where it sits in the feed and what it shows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct LedScreen {
    /// Stable id (8-char hex); given on the way in when left empty.
    #[serde(default)]
    pub uuid: String,
    pub name: String,
    /// The screen's size in LED pixels, as the LED technician gives it.
    pub size: [u32; 2],
    /// Its top-left corner in the feed, in pixels.
    pub at: [i32; 2],
    /// What it shows: `Master` (Main) or a bus's program.
    pub source: OutputSource,
    /// How the picture is put on it.
    pub fit: LedFit,
}

/// How a screen shows its source.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LedFit {
    /// Its part of the source's picture: this rectangle, in the picture's
    /// pixels. Screens taking parts of one source continue one image.
    Part {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// The whole picture, as large as fits; the rest of the screen is black.
    Fit,
    /// The screen covered, the picture's overflow cut off.
    Fill,
    /// The whole picture pulled to the screen's shape.
    Stretch,
}

/// Where a screen lands in the feed and what it samples, as fractions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedPlacement {
    /// `[x, y, width, height]` of the feed.
    pub dest: [f32; 4],
    /// Where sampling starts in the source picture.
    pub uv_offset: [f32; 2],
    /// How much of the source picture the placement spans.
    pub uv_scale: [f32; 2],
}

impl LedFit {
    pub fn label(self) -> &'static str {
        match self {
            Self::Part { .. } => "Part of the picture",
            Self::Fit => "Fit",
            Self::Fill => "Fill",
            Self::Stretch => "Stretch",
        }
    }

    pub fn is_part(self) -> bool {
        matches!(self, Self::Part { .. })
    }
}

impl LedScreen {
    /// A screen of `size` at `at` showing `source` whole, fitted.
    pub fn new(
        name: impl Into<String>,
        size: [u32; 2],
        at: [i32; 2],
        source: OutputSource,
    ) -> Self {
        Self {
            uuid: String::new(),
            name: name.into(),
            size,
            at,
            source,
            fit: LedFit::Fit,
        }
    }

    /// Where this screen lands in a `feed` and what part of its source's
    /// `picture` (pixels) it samples. `None` when nothing of it shows: off
    /// the picture entirely, or of no size.
    pub fn placement(&self, feed: [u32; 2], picture: [u32; 2]) -> Option<LedPlacement> {
        let [fw, fh] = feed.map(|v| v.max(1) as f32);
        let [pw, ph] = picture.map(|v| v.max(1) as f32);
        let [sw, sh] = self.size.map(|v| v as f32);
        if sw < 1.0 || sh < 1.0 {
            return None;
        }
        let (x, y) = (self.at[0] as f32, self.at[1] as f32);
        let whole = ([0.0, 0.0], [1.0, 1.0]);
        let (dest_px, (uv_offset, uv_scale)) = match self.fit {
            LedFit::Stretch => ([x, y, sw, sh], whole),
            LedFit::Fit => {
                let scale = (sw / pw).min(sh / ph);
                let (w, h) = (pw * scale, ph * scale);
                ([x + (sw - w) / 2.0, y + (sh - h) / 2.0, w, h], whole)
            }
            LedFit::Fill => {
                let scale = (sw / pw).max(sh / ph);
                let (u, v) = (sw / scale / pw, sh / scale / ph);
                ([x, y, sw, sh], ([(1.0 - u) / 2.0, (1.0 - v) / 2.0], [u, v]))
            }
            LedFit::Part {
                x: px,
                y: py,
                width,
                height,
            } => {
                let (rw, rh) = (width.max(1) as f32, height.max(1) as f32);
                let (px, py) = (px as f32, py as f32);
                // Only the part inside the picture is drawn: outside it the
                // screen stays black rather than smearing the picture's edge.
                let (x0, y0) = (px.max(0.0), py.max(0.0));
                let (x1, y1) = ((px + rw).min(pw), (py + rh).min(ph));
                if x1 <= x0 || y1 <= y0 {
                    return None;
                }
                let (kx, ky) = (sw / rw, sh / rh);
                (
                    [
                        x + (x0 - px) * kx,
                        y + (y0 - py) * ky,
                        (x1 - x0) * kx,
                        (y1 - y0) * ky,
                    ],
                    ([x0 / pw, y0 / ph], [(x1 - x0) / pw, (y1 - y0) / ph]),
                )
            }
        };
        Some(LedPlacement {
            dest: [
                dest_px[0] / fw,
                dest_px[1] / fh,
                dest_px[2] / fw,
                dest_px[3] / fh,
            ],
            uv_offset,
            uv_scale,
        })
    }

    /// Its rectangle in the feed, `[x, y, width, height]` in pixels.
    pub fn feed_rect(&self) -> [i32; 4] {
        [
            self.at[0],
            self.at[1],
            self.size[0].cast_signed(),
            self.size[1].cast_signed(),
        ]
    }
}

impl LedLayout {
    /// A feed of `feed` pixels with one screen covering it, showing Main.
    pub fn single(feed: [u32; 2]) -> Self {
        Self {
            feed,
            screens: vec![LedScreen::new(
                "Screen 1",
                feed,
                [0, 0],
                OutputSource::Master,
            )],
            test_card: false,
        }
    }

    /// What is wrong with the layout, if anything, in words for the operator.
    pub fn problem(&self) -> Option<String> {
        let in_range = |v: u32| (1..=MAX_LED_PIXELS).contains(&v);
        if !self.feed.iter().all(|&v| in_range(v)) {
            return Some(format!(
                "The feed must be 1 to {MAX_LED_PIXELS} pixels each way"
            ));
        }
        if self.screens.len() > MAX_LED_SCREENS {
            return Some(format!("A feed carries at most {MAX_LED_SCREENS} screens"));
        }
        for screen in &self.screens {
            if !screen.size.iter().all(|&v| in_range(v)) {
                return Some(format!(
                    "'{}' must be 1 to {MAX_LED_PIXELS} pixels each way",
                    screen.name
                ));
            }
            if let LedFit::Part { width, height, .. } = screen.fit {
                if !in_range(width) || !in_range(height) {
                    return Some(format!(
                        "'{}' takes a part of the picture of no size",
                        screen.name
                    ));
                }
            }
            if !matches!(
                screen.source,
                OutputSource::Master | OutputSource::BusProgram(_)
            ) {
                return Some(format!("'{}' can show Main or a bus", screen.name));
            }
        }
        None
    }

    /// Give every screen without an id one from `new_id`, and every screen
    /// without a name "Screen N".
    pub fn fill_in(&mut self, mut new_id: impl FnMut() -> String) {
        for (i, screen) in self.screens.iter_mut().enumerate() {
            if screen.uuid.is_empty() {
                screen.uuid = new_id();
            }
            if screen.name.trim().is_empty() {
                screen.name = format!("Screen {}", i + 1);
            }
        }
    }

    /// Place the screens next to each other in the feed from the top left,
    /// in order, starting a new row when one would run off the right edge.
    pub fn pack_feed(&mut self) {
        let feed_w = self.feed[0].cast_signed();
        let (mut x, mut y, mut row_h) = (0i32, 0i32, 0i32);
        for screen in &mut self.screens {
            let [w, h] = screen.size.map(u32::cast_signed);
            if x > 0 && x + w > feed_w {
                x = 0;
                y += row_h;
                row_h = 0;
            }
            screen.at = [x, y];
            x += w;
            row_h = row_h.max(h);
        }
    }

    /// Lay out, as they stand in a row, the screens taking parts of
    /// `source`'s `picture`: in list order from the left, centred on one
    /// line, one pixel of picture per LED pixel — or smaller, all alike, when
    /// the row would not fit.
    pub fn arrange_parts(&mut self, source: &OutputSource, picture: [u32; 2]) {
        let parts: Vec<usize> = (0..self.screens.len())
            .filter(|&i| self.screens[i].fit.is_part() && &self.screens[i].source == source)
            .collect();
        if parts.is_empty() {
            return;
        }
        let total_w: f64 = parts
            .iter()
            .map(|&i| f64::from(self.screens[i].size[0]))
            .sum();
        let max_h = parts
            .iter()
            .map(|&i| f64::from(self.screens[i].size[1]))
            .fold(0.0, f64::max);
        let [pw, ph] = picture.map(f64::from);
        let scale = (pw / total_w).min(ph / max_h).min(1.0);
        let mut x = (pw - total_w * scale) / 2.0;
        for i in parts {
            let [w, h] = self.screens[i].size.map(|v| f64::from(v) * scale);
            self.screens[i].fit = LedFit::Part {
                x: x.round() as i32,
                y: ((ph - h) / 2.0).round() as i32,
                width: (w.round() as u32).max(1),
                height: (h.round() as u32).max(1),
            };
            x += w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: [u32; 2] = [1920, 1080];

    fn screen(size: [u32; 2], at: [i32; 2], fit: LedFit) -> LedScreen {
        LedScreen {
            fit,
            ..LedScreen::new("S", size, at, OutputSource::Master)
        }
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn a_screen_lands_on_its_own_pixels_of_the_feed() {
        let s = screen([384, 384], [1920, 0], LedFit::Stretch);
        let p = s.placement([3840, 2160], HD).unwrap();
        assert!(close(p.dest, [0.5, 0.0, 0.1, 384.0 / 2160.0]), "{p:?}");
        assert_eq!((p.uv_offset, p.uv_scale), ([0.0, 0.0], [1.0, 1.0]));
    }

    #[test]
    fn fit_letterboxes_and_fill_crops() {
        // A 16:9 picture on a 5:3 wall of 1920×1152.
        let fit = screen([1920, 1152], [0, 0], LedFit::Fit)
            .placement([1920, 1152], HD)
            .unwrap();
        assert!(
            close(fit.dest, [0.0, 36.0 / 1152.0, 1.0, 1080.0 / 1152.0]),
            "{fit:?}"
        );
        let fill = screen([1920, 1152], [0, 0], LedFit::Fill)
            .placement([1920, 1152], HD)
            .unwrap();
        assert!(close(fill.dest, [0.0, 0.0, 1.0, 1.0]));
        // The picture is scaled to 1152 lines: 1800 of its 1920 columns show.
        assert!(
            (fill.uv_scale[0] - 1800.0 / 1920.0).abs() < 1e-5,
            "{fill:?}"
        );
        assert!((fill.uv_scale[1] - 1.0).abs() < 1e-5);
        assert!((fill.uv_offset[0] - 60.0 / 1920.0).abs() < 1e-5);
    }

    #[test]
    fn a_part_samples_its_rectangle_of_the_picture() {
        let s = screen(
            [384, 384],
            [0, 0],
            LedFit::Part {
                x: 192,
                y: 348,
                width: 384,
                height: 384,
            },
        );
        let p = s.placement([384, 384], HD).unwrap();
        assert!(close(p.dest, [0.0, 0.0, 1.0, 1.0]));
        assert!((p.uv_offset[0] - 0.1).abs() < 1e-6 && (p.uv_scale[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_part_hanging_off_the_picture_shows_black_there() {
        // Half of the part is left of the picture: only the right half draws.
        let s = screen(
            [200, 100],
            [0, 0],
            LedFit::Part {
                x: -100,
                y: 0,
                width: 200,
                height: 100,
            },
        );
        let p = s.placement([200, 100], HD).unwrap();
        assert!(close(p.dest, [0.5, 0.0, 0.5, 1.0]), "{p:?}");
        assert_eq!(p.uv_offset, [0.0, 0.0]);
        let off = screen(
            [100, 100],
            [0, 0],
            LedFit::Part {
                x: 5000,
                y: 0,
                width: 100,
                height: 100,
            },
        );
        assert!(off.placement(HD, HD).is_none(), "nothing of it shows");
    }

    #[test]
    fn packing_fills_rows_from_the_top_left() {
        let cube = || screen([384, 384], [9, 9], LedFit::Fit);
        let mut layout = LedLayout {
            feed: HD,
            screens: vec![
                screen([1152, 648], [9, 9], LedFit::Fit),
                cube(),
                cube(),
                cube(),
            ],
            test_card: false,
        };
        layout.pack_feed();
        let at: Vec<[i32; 2]> = layout.screens.iter().map(|s| s.at).collect();
        assert_eq!(
            at,
            [[0, 0], [1152, 0], [1536, 0], [0, 648]],
            "the third ends at the edge, the fourth wraps"
        );
    }

    #[test]
    fn parts_stand_in_a_row_centred_on_the_picture() {
        let part = LedFit::Part {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let mut layout = LedLayout {
            feed: [3840, 2160],
            screens: vec![
                screen([384, 384], [0, 0], part),
                screen([1920, 1152], [0, 0], part),
                screen([384, 384], [0, 0], part),
                screen([500, 500], [0, 0], LedFit::Fit),
            ],
            test_card: false,
        };
        layout.arrange_parts(&OutputSource::Master, [3840, 2160]);
        let rects: Vec<LedFit> = layout.screens.iter().map(|s| s.fit).collect();
        // 2688 wide in all, at one pixel per pixel: 576 from the left.
        let at = |x, y, w, h| LedFit::Part {
            x,
            y,
            width: w,
            height: h,
        };
        assert_eq!(rects[0], at(576, 888, 384, 384));
        assert_eq!(rects[1], at(960, 504, 1920, 1152));
        assert_eq!(rects[2], at(2880, 888, 384, 384));
        assert_eq!(
            rects[3],
            LedFit::Fit,
            "a whole-picture screen is left alone"
        );
        // On a 1080p picture the row is scaled down to fit its height.
        layout.arrange_parts(&OutputSource::Master, HD);
        let LedFit::Part { height, .. } = layout.screens[1].fit else {
            panic!()
        };
        assert!(height <= 1080, "{height}");
    }

    #[test]
    fn only_main_and_buses_and_sane_sizes_pass() {
        let mut layout = LedLayout::single(HD);
        assert_eq!(layout.problem(), None);
        layout.screens[0].source = OutputSource::Channel(0);
        assert!(layout.problem().is_some());
        layout.screens[0].source = OutputSource::BusProgram("b1".into());
        layout.screens[0].size = [0, 10];
        assert!(layout.problem().is_some());
        layout.screens[0].size = [10, 10];
        layout.feed = [0, 1080];
        assert!(layout.problem().is_some());
    }

    #[test]
    fn the_documented_json_reads_as_written() {
        let layout: LedLayout = serde_json::from_str(
            r#"{"feed": [1920, 1080], "screens": [
                {"name": "Wall", "size": [1152, 648], "at": [0, 0], "source": "Master", "fit": {"mode": "fit"}},
                {"name": "Cube", "size": [300, 300], "at": [1152, 0], "source": {"BusProgram": "b1"},
                 "fit": {"mode": "part", "x": 0, "y": 0, "width": 300, "height": 300}}]}"#,
        )
        .unwrap();
        assert!(!layout.test_card);
        assert_eq!(layout.screens[0].fit, LedFit::Fit);
        assert_eq!(
            layout.screens[1].source,
            OutputSource::BusProgram("b1".into())
        );
        assert!(layout.screens[1].fit.is_part());
        assert_eq!(layout.problem(), None);
    }

    #[test]
    fn missing_ids_and_names_are_filled_in() {
        let mut layout = LedLayout::single(HD);
        layout
            .screens
            .push(LedScreen::new(" ", [1, 1], [0, 0], OutputSource::Master));
        let mut n = 0;
        layout.fill_in(|| {
            n += 1;
            format!("id{n}")
        });
        assert_eq!(layout.screens[0].uuid, "id1");
        assert_eq!(layout.screens[1].name, "Screen 2");
    }
}
