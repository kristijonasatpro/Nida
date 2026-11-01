//! The screens a show's window can go on, and the one it goes on.
//!
//! PowerPoint draws a windowed show's slide the size of its window, and
//! macOS keeps a window inside its screen — below the menu bar, above the
//! Dock. So a spare screen (connected, no output on it) has room for a
//! bigger slide than this Mac's own, and a Retina one for a sharper one.

use std::collections::HashSet;

use super::powerpoint::Placement;

/// The menu bar's height, points: a window is kept below it (30 on macOS
/// 27, measured). Every screen has one while displays have separate Spaces,
/// as they do unless turned off.
const MENU_BAR: i32 = 30;

/// The Dock's, on the main screen (measured: a window kept 974 high on a
/// screen 1080 high).
const DOCK: i32 = 76;

/// A connected screen, as a show's window would go on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenSpot {
    /// The platform's number for it (a `CGDirectDisplayID`).
    pub display: u32,
    /// Its place and size on the desktop, points: the main screen's top-left
    /// at 0, 0, y going down — as PowerPoint places a window.
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    /// Its pixels to a point: 2 on a Retina screen.
    pub scale: f64,
    /// The screen with the menu bar and the Dock.
    pub main: bool,
}

impl ScreenSpot {
    /// This Mac's screen when the screens cannot be read: at 0, 0, `scale`
    /// pixels to a point.
    #[must_use]
    pub fn main_at(scale: f64) -> Self {
        Self {
            display: 0,
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
            scale,
            main: true,
        }
    }

    /// Whether `window` is on this screen: its middle is.
    #[must_use]
    pub fn holds(&self, window: Placement) -> bool {
        let x = window.left + window.width / 2;
        let y = window.top + window.height / 2;
        (self.left..self.left + self.width).contains(&x)
            && (self.top..self.top + self.height).contains(&y)
    }

    /// The slide's width and height, pixels, that a show's window on this
    /// screen shows at most, for a picture `pixels` big: the slide's shape
    /// (`aspect`, width over height) in what is left below the menu bar,
    /// above the Dock on the main screen and under the window's title bar
    /// (`title_bar` points), at the screen's pixels — no bigger than the
    /// slide in the picture.
    #[must_use]
    pub fn slide_pixels(&self, pixels: (u32, u32), aspect: f64, title_bar: u32) -> (u32, u32) {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            16.0 / 9.0
        };
        let fit = |w: f64, h: f64| {
            if w / h > aspect {
                (h * aspect, h)
            } else {
                (w, w / aspect)
            }
        };
        let bar = i32::try_from(title_bar).unwrap_or(0);
        let dock = if self.main { DOCK } else { 0 };
        let room = fit(
            f64::from(self.width.max(1)),
            f64::from((self.height - MENU_BAR - dock - bar).max(1)),
        );
        let scale = if self.scale.is_finite() && self.scale >= 1.0 {
            self.scale
        } else {
            1.0
        };
        let (w, h) = (room.0 * scale, room.1 * scale);
        let most = fit(f64::from(pixels.0.max(1)), f64::from(pixels.1.max(1)));
        let shrink = (most.0 / w).min(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        ((w * shrink).round() as u32, (h * shrink).round() as u32)
    }
}

/// Where a show is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Where it plays sharpest; a spare screen when as sharp as the main.
    Auto,
    /// This Mac's screen.
    Main,
    /// That screen, while connected and free; else as `Auto`.
    Display(u32),
}

/// The screen a show plays on, of the connected `screens`: as `want` says,
/// those in `taken` — an output is on it, or a window put there did not
/// stay — left out, the main screen never. For `Auto`, the one whose slide
/// comes out biggest; of two as big, the spare one (this Mac's screen kept
/// for Nida), then the one macOS numbers lower — the same one each time.
/// `None` only with no screens.
#[must_use]
pub fn choose<S: std::hash::BuildHasher>(
    screens: &[ScreenSpot],
    want: Want,
    taken: &HashSet<u32, S>,
    pixels: (u32, u32),
    aspect: f64,
    title_bar: u32,
) -> Option<ScreenSpot> {
    let main = screens.iter().find(|s| s.main).or(screens.first()).copied();
    let free = |s: &&ScreenSpot| s.main || !taken.contains(&s.display);
    match want {
        Want::Main => return main,
        Want::Display(display) => {
            if let Some(screen) = screens.iter().filter(free).find(|s| s.display == display) {
                return Some(*screen);
            }
        }
        Want::Auto => {}
    }
    screens
        .iter()
        .filter(free)
        .max_by_key(|s| {
            (
                s.slide_pixels(pixels, aspect, title_bar).0,
                !s.main,
                std::cmp::Reverse(s.display),
            )
        })
        .copied()
        .or(main)
}

/// The screen with the menu bar (a `CGDirectDisplayID`); `None` but on a Mac.
#[must_use]
pub fn main_display() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        Some(objc2_core_graphics::CGMainDisplayID())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The connected screens that show a picture of their own — one mirroring
/// another left out — the main one marked.
#[must_use]
pub fn connected() -> Vec<ScreenSpot> {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_graphics::{
            CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMirrorsDisplay, CGDisplayMode,
            CGError, CGGetActiveDisplayList, CGMainDisplayID,
        };
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        #[allow(clippy::cast_possible_truncation)]
        // SAFETY: both pointers are valid, `ids` for the length given.
        let got =
            unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &raw mut count) };
        if got != CGError::Success {
            return Vec::new();
        }
        let main = CGMainDisplayID();
        ids.iter()
            .take(count as usize)
            .copied()
            .filter(|id| CGDisplayMirrorsDisplay(*id) == 0)
            .map(|id| {
                let bounds = CGDisplayBounds(id);
                #[allow(clippy::cast_precision_loss)]
                let scale = CGDisplayCopyDisplayMode(id).map_or(1.0, |mode| {
                    let points = CGDisplayMode::width(Some(&mode));
                    let pixels = CGDisplayMode::pixel_width(Some(&mode));
                    if points > 0 {
                        pixels as f64 / points as f64
                    } else {
                        1.0
                    }
                });
                #[allow(clippy::cast_possible_truncation)]
                ScreenSpot {
                    display: id,
                    left: bounds.origin.x.round() as i32,
                    top: bounds.origin.y.round() as i32,
                    width: bounds.size.width.round() as i32,
                    height: bounds.size.height.round() as i32,
                    scale: scale.max(1.0),
                    main: id == main,
                }
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: (u32, u32) = (1920, 1080);
    const WIDE: f64 = 16.0 / 9.0;

    fn screen(display: u32, left: i32, size: (i32, i32), scale: f64, main: bool) -> ScreenSpot {
        ScreenSpot {
            display,
            left,
            top: 0,
            width: size.0,
            height: size.1,
            scale,
            main,
        }
    }

    /// The mini's: a 1080p screen at 1×, the menu bar's.
    fn mini() -> ScreenSpot {
        screen(1, 0, (1920, 1080), 1.0, true)
    }

    #[test]
    fn a_slide_on_a_screen_is_what_its_menu_bar_dock_and_title_bar_leave() {
        // Measured on the mini: 1675 × 942 under the title bar.
        assert_eq!(mini().slide_pixels(HD, WIDE, 32), (1675, 942));
        // A spare 1080p screen has no Dock: a little bigger.
        let spare = screen(2, 1920, (1920, 1080), 1.0, false);
        assert_eq!(spare.slide_pixels(HD, WIDE, 32), (1810, 1018));
        // A 4K screen at 2× (looks like 1920 × 1080): the whole picture.
        let sharp = screen(3, 1920, (1920, 1080), 2.0, false);
        assert_eq!(sharp.slide_pixels(HD, WIDE, 32), (1920, 1080));
        // A 1440p screen: room for it at 1×.
        let big = screen(4, 1920, (2560, 1440), 1.0, false);
        assert_eq!(big.slide_pixels(HD, WIDE, 32), (1920, 1080));
        // A 4K picture from a 4K screen looking like 2560 × 1440.
        assert_eq!(
            screen(5, 1920, (2560, 1440), 2.0, false).slide_pixels((3840, 2160), WIDE, 32),
            (3840, 2160)
        );
        // A 4:3 slide in a 16:9 picture: no wider than it is there.
        assert_eq!(big.slide_pixels(HD, 4.0 / 3.0, 32), (1440, 1080));
    }

    #[test]
    fn automatic_takes_the_screen_it_plays_sharpest_on() {
        let none = HashSet::new();
        let choose = |screens: &[ScreenSpot], taken: &HashSet<u32>| {
            choose(screens, Want::Auto, taken, HD, WIDE, 32).map(|s| s.display)
        };
        // This Mac's screen alone: there.
        assert_eq!(choose(&[mini()], &none), Some(1));
        // A spare 1080p screen beside it: bigger there.
        let spare = screen(2, 1920, (1920, 1080), 1.0, false);
        assert_eq!(choose(&[mini(), spare], &none), Some(2));
        // An output on it: not there.
        assert_eq!(choose(&[mini(), spare], &HashSet::from([2])), Some(1));
        // A Retina MacBook (1512 × 982 at 2×) is sharper than a spare 1080p
        // screen at 1×: kept there.
        let macbook = screen(1, 0, (1512, 982), 2.0, true);
        assert_eq!(choose(&[macbook, spare], &none), Some(1));
        // As sharp on a spare 4K screen: there, this Mac's screen kept for Nida.
        let sharp = screen(3, 1512, (1920, 1080), 2.0, false);
        assert_eq!(choose(&[macbook, sharp], &none), Some(3));
        // Of two spare screens as good, the same each time: the lower number.
        let other = screen(4, 3432, (1920, 1080), 2.0, false);
        assert_eq!(choose(&[macbook, other, sharp], &none), Some(3));
        // A small one (a 720p TV) is worse than this Mac's.
        let small = screen(5, 1920, (1280, 720), 1.0, false);
        assert_eq!(choose(&[mini(), small], &none), Some(1));
        assert_eq!(choose(&[], &none), None);
    }

    #[test]
    fn a_screen_chosen_is_taken_while_it_is_free() {
        let spare = screen(2, 1920, (1920, 1080), 1.0, false);
        let small = screen(5, -1280, (1280, 720), 1.0, false);
        let screens = [mini(), spare, small];
        let none = HashSet::new();
        let pick = |want: Want, taken: &HashSet<u32>| {
            choose(&screens, want, taken, HD, WIDE, 32).map(|s| s.display)
        };
        assert_eq!(pick(Want::Display(5), &none), Some(5));
        assert_eq!(pick(Want::Main, &none), Some(1));
        // Gone, or an output on it: as automatic.
        assert_eq!(pick(Want::Display(9), &none), Some(2));
        assert_eq!(pick(Want::Display(5), &HashSet::from([5])), Some(2));
        // This Mac's screen, even with an output said to be on it.
        assert_eq!(pick(Want::Display(1), &HashSet::from([1])), Some(1));
    }

    #[test]
    fn a_window_is_on_the_screen_its_middle_is_on() {
        let spare = screen(2, 1920, (1920, 1080), 1.0, false);
        let at = |left: i32, top: i32| Placement {
            left,
            top,
            width: 1810,
            height: 1050,
        };
        assert!(spare.holds(at(1920, 30)));
        assert!(!spare.holds(at(0, 30)));
        assert!(!mini().holds(at(1920, 30)));
        // Pushed half off by macOS: where most of it is.
        assert!(spare.holds(at(1400, 30)));
        // A screen left of the main one, at negative x.
        let left = screen(5, -1280, (1280, 720), 1.0, false);
        assert!(left.holds(Placement {
            left: -1280,
            top: 30,
            width: 1150,
            height: 650
        }));
    }
}
