//! Crop/scale geometry and CPU resampling shared by the platform backends.
//!
//! macOS is the exception: `SCStreamConfiguration` crops and scales inside
//! `ScreenCaptureKit`, so a 4K display never crosses the process boundary at
//! full size. Windows Graphics Capture, X11, and `PipeWire` all hand back a
//! full-size surface, so the same crop-then-downscale has to happen here. One
//! implementation, so a fix to the filter is a fix on every platform.
//!
//! See spec/screen-capture.md § Platform Support.

use super::backend::{CaptureConfig, Drawn};

/// A resolved source rectangle and delivered output size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub out_w: u32,
    pub out_h: u32,
}

impl Geometry {
    /// Resolve a config against the target's native size.
    ///
    /// Crop selects a sub-rectangle of the captured surface, which genuinely
    /// shrinks the readback because backends copy only that region. `scale_to`
    /// then sets the delivered size — as big as the picture is `drawn` in it
    /// (see [`drawn_within`]) — preserving the cropped aspect ratio so the
    /// deck's own scaling mode still has something sane to letterbox. Upscaling
    /// is refused: enlarging on the CPU costs bandwidth and adds no detail.
    pub fn resolve(native_w: u32, native_h: u32, config: &CaptureConfig) -> Self {
        let (nw, nh) = (native_w.max(1), native_h.max(1));
        let crop = config.crop.clamped();
        // The last pixel at most: an origin rounded up to the far side would
        // put the part, a pixel wide at least, outside the surface.
        let src_x = (((nw as f32) * crop.x).round() as u32).min(nw - 1);
        let src_y = (((nh as f32) * crop.y).round() as u32).min(nh - 1);
        let src_w =
            (((nw as f32) * crop.w).round() as u32).clamp(1, nw.saturating_sub(src_x).max(1));
        let src_h =
            (((nh as f32) * crop.h).round() as u32).clamp(1, nh.saturating_sub(src_y).max(1));
        let (out_w, out_h) = config
            .scale_to
            .map_or((src_w, src_h), |(w, h)| {
                drawn_within(src_w, src_h, w, h, config.drawn)
            });
        Self {
            src_x,
            src_y,
            src_w,
            src_h,
            out_w,
            out_h,
        }
    }

    /// Whether the resolved output already matches the source rectangle, in
    /// which case the caller can skip [`downscale`] entirely.
    pub fn is_identity_scale(self) -> bool {
        self.out_w == self.src_w && self.out_h == self.src_h
    }
}

/// Largest size with `w:h`'s aspect ratio that fits inside `max_w × max_h`,
/// never enlarging.
///
/// Also the right value to put in [`CaptureConfig::scale_to`] when opening a
/// capture for a deck: pass the target's native size and the deck's size. The
/// deck's size alone would be wrong, because backends treat `scale_to` as the
/// delivered extent — a window shaped differently from the stage would arrive
/// already fitted to the deck's aspect (letterboxed by `ScreenCaptureKit`, which
/// bakes the bars into the pixels), leaving the deck's own scaling mode with
/// identical source and target dimensions and therefore nothing to do.
pub fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if max_w == 0 || max_h == 0 {
        return (w, h);
    }
    let scale = (max_w as f32 / w as f32).min(max_h as f32 / h as f32);
    if scale >= 1.0 {
        return (w, h);
    }
    (
        (((w as f32) * scale).round() as u32).max(1),
        (((h as f32) * scale).round() as u32).max(1),
    )
}

/// `w × h` at the size it is `drawn` in a frame `max_w × max_h`, never
/// enlarged: so many times the size that fits within the frame, or its
/// width or height so many times the frame's, whichever is most. Fitted
/// once ([`Drawn::default`]) it is [`fit_within`].
///
/// A picture that fills a frame of another shape comes with the frame's own
/// pixels along the side that fills it — a 4K screen in an upright
/// 1080 × 1920 frame 1920 high, not the 608 that fit within it — and one
/// zoomed into with those the zoom shows.
pub fn drawn_within(w: u32, h: u32, max_w: u32, max_h: u32, drawn: Drawn) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if max_w == 0 || max_h == 0 {
        return (w, h);
    }
    let (across, down) = (max_w as f32 / w as f32, max_h as f32 / h as f32);
    let fits = across.min(down);
    let drawn = drawn.sanitized();
    let scale = (fits * drawn.fitted)
        .max(across * drawn.across)
        .max(down * drawn.down);
    if scale >= 1.0 {
        return (w, h);
    }
    (
        (((w as f32) * scale).round() as u32).max(1),
        (((h as f32) * scale).round() as u32).max(1),
    )
}

/// Box-filter downscale of a tightly-packed 4-bytes-per-pixel image.
///
/// Averaging rather than nearest-neighbour matters here: desktop content is
/// full of one-pixel text stems, and point sampling a 4K display down to deck
/// resolution makes them shimmer as the source scrolls. Channel order is
/// irrelevant, so this serves both RGBA and BGRA callers.
pub fn downscale(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let (dst_w, dst_h) = (dst_w.max(1), dst_h.max(1));
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
    let x_ratio = src_w as f32 / dst_w as f32;
    let y_ratio = src_h as f32 / dst_h as f32;
    for y in 0..dst_h {
        let y0 = ((y as f32) * y_ratio) as u32;
        let y1 = ((((y + 1) as f32) * y_ratio) as u32).clamp(y0 + 1, src_h);
        for x in 0..dst_w {
            let x0 = ((x as f32) * x_ratio) as u32;
            let x1 = ((((x + 1) as f32) * x_ratio) as u32).clamp(x0 + 1, src_w);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                let row = (sy as usize) * (src_w as usize) * 4;
                for sx in x0..x1 {
                    let i = row + (sx as usize) * 4;
                    if i + 3 < src.len() {
                        acc[0] += u32::from(src[i]);
                        acc[1] += u32::from(src[i + 1]);
                        acc[2] += u32::from(src[i + 2]);
                        acc[3] += u32::from(src[i + 3]);
                        n += 1;
                    }
                }
            }
            let o = ((y as usize) * (dst_w as usize) + (x as usize)) * 4;
            let n = n.max(1);
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = (acc[3] / n) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen_capture::backend::CropRect;

    #[test]
    fn full_frame_config_resolves_to_the_native_rect() {
        let g = Geometry::resolve(1920, 1080, &CaptureConfig::default());
        assert_eq!((g.src_x, g.src_y, g.src_w, g.src_h), (0, 0, 1920, 1080));
        assert_eq!((g.out_w, g.out_h), (1920, 1080));
        assert!(g.is_identity_scale());
    }

    #[test]
    fn crop_selects_a_sub_rect_that_stays_inside_the_surface() {
        let cfg = CaptureConfig {
            crop: CropRect {
                x: 0.5,
                y: 0.5,
                w: 0.75,
                h: 0.75,
            },
            ..Default::default()
        };
        let g = Geometry::resolve(1000, 1000, &cfg);
        assert_eq!((g.src_x, g.src_y), (500, 500));
        assert!(g.src_x + g.src_w <= 1000, "crop ran past the right edge");
        assert!(g.src_y + g.src_h <= 1000, "crop ran past the bottom edge");
    }

    /// A crop that begins within half a pixel of the far side begins on the
    /// last pixel, not past it: its part, a pixel at least, is in the surface.
    #[test]
    fn a_crop_from_the_far_side_begins_on_the_last_pixel() {
        let far = 4095.0 / 4096.0;
        for (x, w) in [(far, 1.0 - far), (1.0, 0.5)] {
            let cfg = CaptureConfig {
                crop: CropRect {
                    x,
                    y: x,
                    w,
                    h: w,
                },
                ..Default::default()
            };
            for (nw, nh) in [(1499, 827), (1920, 1080), (1, 1)] {
                let g = Geometry::resolve(nw, nh, &cfg);
                assert_eq!(
                    (g.src_x, g.src_y, g.src_w, g.src_h),
                    (nw - 1, nh - 1, 1, 1),
                    "{nw}x{nh} from {x}"
                );
            }
        }
    }

    #[test]
    fn scale_to_preserves_the_cropped_aspect_ratio() {
        let cfg = CaptureConfig {
            scale_to: Some((640, 640)),
            ..Default::default()
        };
        let g = Geometry::resolve(1920, 1080, &cfg);
        assert_eq!((g.out_w, g.out_h), (640, 360));
    }

    #[test]
    fn scale_to_never_upscales() {
        let cfg = CaptureConfig {
            scale_to: Some((4096, 4096)),
            ..Default::default()
        };
        let g = Geometry::resolve(640, 480, &cfg);
        assert_eq!((g.out_w, g.out_h), (640, 480));
        assert!(g.is_identity_scale());
    }

    #[test]
    fn crop_shrinks_the_source_rect_so_the_readback_shrinks_with_it() {
        let cfg = CaptureConfig {
            crop: CropRect {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.5,
            },
            ..Default::default()
        };
        let g = Geometry::resolve(1920, 1080, &cfg);
        assert_eq!((g.src_x, g.src_y), (480, 540));
        assert_eq!((g.src_w, g.src_h), (960, 540));
        // This is the whole bandwidth argument: a crop must not read the full frame.
        assert!(g.src_w * g.src_h < 1920 * 1080);
    }

    #[test]
    fn scale_to_preserves_aspect_after_an_asymmetric_crop() {
        let cfg = CaptureConfig {
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        let g = Geometry::resolve(3840, 2160, &cfg);
        let src_aspect = g.src_w as f32 / g.src_h as f32;
        let out_aspect = g.out_w as f32 / g.out_h as f32;
        assert!(
            (src_aspect - out_aspect).abs() < 0.01,
            "aspect drifted: {src_aspect} vs {out_aspect}"
        );
    }

    #[test]
    fn fit_within_keeps_a_windows_shape_instead_of_the_decks() {
        // The bug this guards: opening a 1000×800 window for a 16:9 deck used to
        // pass the deck size straight through as scale_to, so the capture came
        // back already fitted to 16:9 and the deck's scaling mode had identical
        // source and target dimensions — every mode collapsed to identity.
        let (w, h) = fit_within(1000, 800, 1920, 1080);
        let native_aspect = 1000.0 / 800.0;
        let capped_aspect = w as f32 / h as f32;
        assert!(
            (native_aspect - capped_aspect).abs() < 0.01,
            "capture must keep the window's shape, got {w}×{h}"
        );
        assert_ne!(
            (w, h),
            (1920, 1080),
            "a smaller window must not be blown up to the deck size"
        );
    }

    #[test]
    fn fit_within_caps_a_larger_target_to_the_deck() {
        // The bandwidth argument still has to hold: 4K must not arrive at 4K.
        assert_eq!(fit_within(3840, 2160, 1920, 1080), (1920, 1080));
        // An ultrawide is bounded by width and keeps its shape rather than
        // being squashed into the deck's 16:9.
        assert_eq!(fit_within(3440, 1440, 1920, 1080), (1920, 804));
    }

    /// Fitted once, drawn is fitted: every size as it came before.
    #[test]
    fn drawn_fitted_once_is_fit_within() {
        let sizes = [
            (3840, 2160, 1920, 1080),
            (3440, 1440, 1920, 1080),
            (1000, 800, 1920, 1080),
            (1920, 1080, 2160, 3840),
            (3840, 2160, 1080, 1920),
            (5120, 2880, 1366, 768),
            (640, 480, 4096, 4096),
            (640, 480, 0, 0),
            (0, 0, 1920, 1080),
            (1, 4000, 1920, 1080),
        ];
        for (w, h, max_w, max_h) in sizes {
            assert_eq!(
                drawn_within(w, h, max_w, max_h, Drawn::default()),
                fit_within(w, h, max_w, max_h),
                "{w}×{h} in {max_w}×{max_h}"
            );
        }
    }

    /// A picture that fills a frame of another shape comes with the pixels
    /// that fill it: fitted — how it was — a 4K screen filling an upright
    /// Full HD frame came 1080 × 608 and was enlarged over three times.
    #[test]
    fn drawn_filling_a_frame_comes_with_the_pixels_that_fill_it() {
        let fills = Drawn {
            fitted: 1.0,
            across: 1.0,
            down: 1.0,
        };
        assert_eq!(fit_within(3840, 2160, 1080, 1920), (1080, 608));
        assert_eq!(drawn_within(3840, 2160, 1080, 1920, fills), (3413, 1920));
        // In an upright 4K frame, every pixel the screen has: no more.
        assert_eq!(drawn_within(3840, 2160, 2160, 3840, fills), (3840, 2160));
        assert_eq!(drawn_within(1920, 1080, 2160, 3840, fills), (1920, 1080));
        // A frame of its own shape is filled as it is fitted.
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, fills), (1920, 1080));
        // A 4:3 picture in a 16:9 frame: the frame's 1920 across, its top
        // and bottom cut.
        assert_eq!(fit_within(2880, 2160, 1920, 1080), (1440, 1080));
        assert_eq!(drawn_within(2880, 2160, 1920, 1080, fills), (1920, 1440));
        // Filling counts alone, where nothing shows it fitted.
        let only_fills = Drawn {
            fitted: 0.0,
            across: 1.0,
            down: 1.0,
        };
        assert_eq!(drawn_within(2880, 2160, 1920, 1080, only_fills), (1920, 1440));
    }

    /// Stretched to a frame and scaled apart, a picture comes with the pixels
    /// the side that shows the most of them wants — not as if both were
    /// scaled by the bigger: a 4K screen stretched to an upright Full HD
    /// frame, twice as wide and half as high, would come whole, three times
    /// the pixels shown.
    #[test]
    fn drawn_stretched_comes_with_the_pixels_its_sides_show() {
        let stretched = |across, down| Drawn {
            fitted: 1.0,
            across,
            down,
        };
        // Its width 2160 of the screen's 3840 — more than its height wants.
        assert_eq!(drawn_within(3840, 2160, 1080, 1920, stretched(2.0, 0.5)), (2160, 1215));
        // Its height the frame's 1920 — more than its width wants.
        assert_eq!(drawn_within(3840, 2160, 1080, 1920, stretched(0.5, 1.0)), (3413, 1920));
        // Each side by itself.
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, stretched(1.5, 0.0)), (2880, 1620));
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, stretched(0.0, 1.5)), (2880, 1620));
        // Smaller both ways than fitted: as fitted.
        assert_eq!(drawn_within(3840, 2160, 1080, 1920, stretched(0.5, 0.1)), (1080, 608));
    }

    /// Zoomed into, a picture comes with the pixels the zoom shows, as far
    /// as its screen has them: fitted — how it was — a 4K screen zoomed into
    /// twice over in a 1080p show came 1920 × 1080 and was enlarged.
    #[test]
    fn drawn_zoomed_into_comes_with_the_pixels_the_zoom_shows() {
        let zoomed = |fitted| Drawn {
            fitted,
            ..Drawn::default()
        };
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, zoomed(1.0)), (1920, 1080));
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, zoomed(1.5)), (2880, 1620));
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, zoomed(2.0)), (3840, 2160));
        assert_eq!(drawn_within(3840, 2160, 1920, 1080, zoomed(5.0)), (3840, 2160));
        // As far as a Transform scales — 5, and 5 again an axis — in a frame
        // however small.
        assert_eq!(drawn_within(3840, 2160, 128, 72, zoomed(16.0)), (2048, 1152));
        assert_eq!(drawn_within(3840, 2160, 128, 72, zoomed(25.0)), (3200, 1800));
        // Zoomed into while it fills: times the size that fills.
        let filled_twice = Drawn {
            fitted: 1.0,
            across: 2.0,
            down: 2.0,
        };
        assert_eq!(drawn_within(5760, 2160, 1920, 1080, filled_twice), (5760, 2160));
        assert_eq!(drawn_within(11520, 4320, 1920, 1080, filled_twice), (5760, 2160));
        // No size, or none that is a number: as fitted. No frame: as it is.
        for none in [zoomed(0.0), zoomed(f32::NAN), zoomed(-2.0)] {
            assert_eq!(drawn_within(3840, 2160, 1920, 1080, none), (1920, 1080));
        }
        assert_eq!(drawn_within(3840, 2160, 0, 0, zoomed(2.0)), (3840, 2160));
    }

    /// The crop is taken first, and what it keeps sized as it is drawn: the
    /// left half of a 4K screen, upright, filling a 1080p show.
    #[test]
    fn a_crop_is_sized_as_it_is_drawn() {
        let half = |drawn| CaptureConfig {
            scale_to: Some((1920, 1080)),
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            drawn,
            ..Default::default()
        };
        let sized = |drawn| {
            let g = Geometry::resolve(3840, 2160, &half(drawn));
            assert_eq!((g.src_w, g.src_h), (1920, 2160));
            (g.out_w, g.out_h)
        };
        assert_eq!(sized(Drawn::default()), (960, 1080));
        let fills = Drawn {
            fitted: 1.0,
            across: 1.0,
            down: 1.0,
        };
        assert_eq!(sized(fills), (1920, 2160));
        let zoomed = Drawn {
            fitted: 1.5,
            ..Drawn::default()
        };
        assert_eq!(sized(zoomed), (1440, 1620));
    }

    #[test]
    fn fit_within_never_upscales_or_divides_by_zero() {
        assert_eq!(fit_within(640, 480, 4096, 4096), (640, 480));
        assert_eq!(fit_within(640, 480, 0, 0), (640, 480));
        assert_eq!(fit_within(0, 0, 1920, 1080), (1, 1));
    }

    #[test]
    fn geometry_is_never_degenerate() {
        let cfg = CaptureConfig {
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.0001,
                h: 0.0001,
            },
            ..Default::default()
        };
        let g = Geometry::resolve(1920, 1080, &cfg);
        assert!(g.src_w >= 1 && g.src_h >= 1);
        assert!(g.out_w >= 1 && g.out_h >= 1);
    }

    #[test]
    fn downscale_halving_averages_rather_than_dropping_pixels() {
        let src: Vec<u8> = vec![
            0, 0, 0, 255, 100, 100, 100, 255, //
            200, 200, 200, 255, 255, 255, 255, 255,
        ];
        let out = downscale(&src, 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        assert_eq!(u32::from(out[0]), (100 + 200 + 255) / 4);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn downscale_output_is_tightly_packed() {
        let src = vec![0u8; 8 * 8 * 4];
        let out = downscale(&src, 8, 8, 3, 5);
        assert_eq!(out.len(), 3 * 5 * 4, "manager requires width*4 rows");
    }

    #[test]
    fn downscale_never_reads_past_a_short_source() {
        // A truncated readback must not panic — the backend clamps and delivers
        // what it has rather than taking the whole app down.
        let src = vec![7u8; 4 * 4 * 4 / 2];
        let out = downscale(&src, 4, 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
    }
}
