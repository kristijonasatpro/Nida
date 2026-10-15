//! Picking a colour off the screen — anywhere, Nida's own pictures
//! included — with macOS's colour sampler, the magnifier the system colour
//! panel uses. Elsewhere there is none yet ([`available`] is false).
//!
//! One pick at a time, for one colour field: [`start`] opens the sampler,
//! and the field takes the colour with [`take`] on a later frame, once the
//! sampler has closed with one.

use std::sync::Mutex;

/// How far a pick has got.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Picking,
    /// This colour, straight linear RGB.
    Picked([f32; 3]),
    /// Esc, or no colour to be had.
    Cancelled,
}

/// A pick under way or finished, and the field it is for.
struct Pick {
    field: egui::Id,
    stage: Stage,
}

static PICK: Mutex<Option<Pick>> = Mutex::new(None);

/// Whether the screen can be sampled here.
pub fn available() -> bool {
    cfg!(target_os = "macos")
}

/// Open the sampler for the colour field `field`. Esc closes it with none.
pub fn start(field: egui::Id) {
    if let Ok(mut pick) = PICK.lock() {
        *pick = Some(Pick {
            field,
            stage: Stage::Picking,
        });
    }
    #[cfg(target_os = "macos")]
    macos::show_sampler();
    #[cfg(not(target_os = "macos"))]
    finish(None);
}

/// Whether a pick for `field` is under way.
pub fn picking(field: egui::Id) -> bool {
    PICK.lock()
        .ok()
        .and_then(|pick| {
            pick.as_ref()
                .map(|p| p.field == field && p.stage == Stage::Picking)
        })
        .unwrap_or(false)
}

/// The colour picked for `field`, straight linear RGB — once, as the sampler
/// closes with one.
pub fn take(field: egui::Id) -> Option<[f32; 3]> {
    let mut pick = PICK.lock().ok()?;
    let done = pick
        .as_ref()
        .is_some_and(|p| p.field == field && p.stage != Stage::Picking);
    if !done {
        return None;
    }
    match pick.take()?.stage {
        Stage::Picked(rgb) => Some(rgb),
        Stage::Picking | Stage::Cancelled => None,
    }
}

/// The sampler closed, with a colour (sRGB, as the screen shows it) or none.
fn finish(srgb: Option<[f32; 3]>) {
    if let Ok(mut pick) = PICK.lock() {
        if let Some(p) = pick.as_mut() {
            p.stage = match srgb {
                Some(c) => Stage::Picked(c.map(linear_from_srgb)),
                None => Stage::Cancelled,
            };
        }
    }
}

/// The screen shows sRGB; colours in Nida are linear light.
fn linear_from_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::cell::RefCell;

    thread_local! {
        /// The sampler showing, kept until the next pick.
        static SAMPLER: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    /// Show `NSColorSampler`; its handler runs on the main thread with the
    /// colour clicked, or nil for Esc.
    pub(super) fn show_sampler() {
        // SAFETY: NSColorSampler (macOS 10.15+) is an AppKit class, shown
        // from the main thread the UI runs on; its handler takes one
        // nullable NSColor, which is what the block is typed as.
        unsafe {
            let sampler: Retained<AnyObject> = msg_send![class!(NSColorSampler), new];
            let handler = RcBlock::new(|color: *mut AnyObject| {
                let srgb = if color.is_null() {
                    None
                } else {
                    srgb_components(color)
                };
                super::finish(srgb);
            });
            let () = msg_send![&*sampler, showSamplerWithSelectionHandler: &*handler];
            SAMPLER.with(|s| *s.borrow_mut() = Some(sampler));
        }
    }

    /// The colour's red, green and blue in sRGB.
    ///
    /// # Safety
    ///
    /// `color` must be a live `NSColor`.
    unsafe fn srgb_components(color: *mut AnyObject) -> Option<[f32; 3]> {
        let space: *mut AnyObject = msg_send![class!(NSColorSpace), sRGBColorSpace];
        let srgb: *mut AnyObject = msg_send![color, colorUsingColorSpace: space];
        if srgb.is_null() {
            return None;
        }
        let r: f64 = msg_send![srgb, redComponent];
        let g: f64 = msg_send![srgb, greenComponent];
        let b: f64 = msg_send![srgb, blueComponent];
        Some([r as f32, g as f32, b as f32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sampler and the colour calls are there, taking what they are
    /// sent — checked without showing the sampler.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_has_the_sampler_and_the_calls_fit_it() {
        use objc2::runtime::AnyClass;
        use objc2::sel;
        let sampler = AnyClass::get(c"NSColorSampler").expect("NSColorSampler");
        let show = sampler
            .instance_method(sel!(showSamplerWithSelectionHandler:))
            .expect("showSamplerWithSelectionHandler:");
        assert_eq!(
            show.argument_type(2).as_deref(),
            Some(c"@?"),
            "takes a block"
        );
        let space = AnyClass::get(c"NSColorSpace").expect("NSColorSpace");
        assert!(space.class_method(sel!(sRGBColorSpace)).is_some());
        let color = AnyClass::get(c"NSColor").expect("NSColor");
        assert!(color.instance_method(sel!(colorUsingColorSpace:)).is_some());
        let red = color
            .instance_method(sel!(redComponent))
            .expect("redComponent");
        assert_eq!(&*red.return_type(), c"d", "a CGFloat");
    }

    /// As `start`, without showing the sampler.
    fn start_picking_for(field: egui::Id) {
        *PICK.lock().unwrap() = Some(Pick {
            field,
            stage: Stage::Picking,
        });
    }

    #[test]
    fn a_picked_colour_goes_to_its_field_once_in_linear_light_and_esc_picks_none() {
        let field = egui::Id::new("test colour field");
        let other = egui::Id::new("another field");
        start_picking_for(field);
        assert!(picking(field));
        assert!(!picking(other));
        assert_eq!(take(field), None, "nothing yet");
        finish(Some([1.0, 0.5, 0.0]));
        assert!(!picking(field));
        assert_eq!(take(other), None, "not for another field");
        let rgb = take(field).expect("the colour");
        assert!((rgb[0] - 1.0).abs() < 1e-6 && rgb[2].abs() < 1e-6);
        assert!(
            (rgb[1] - 0.214).abs() < 1e-3,
            "sRGB 0.5 is linear 0.214: {rgb:?}"
        );
        assert_eq!(take(field), None, "once");

        // Esc leaves the colour as it was. (One test: the pick is global.)
        let field = egui::Id::new("cancelled field");
        start_picking_for(field);
        assert!(picking(field));
        finish(None);
        assert_eq!(take(field), None);
        assert!(!picking(field));
    }
}
