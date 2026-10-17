//! The control window's screen kept at the show's rate while Nida runs.
//!
//! A `MacBook` Pro's screen refreshes up to 120 times a second (`ProMotion`).
//! With the control window on it and a screen captured at 60 fps, macOS's
//! window server — compositing that screen twice for each show frame —
//! missed refreshes on every screen: the LED wall 8 times in 20 s, the
//! control window 21. The same screen set to 60 Hz: none, and one (a laptop,
//! 2026-10-02). Video and show apps are told to run their screens at a fixed
//! rate equal to the show's (Apple's own advice for video; Resolume's and
//! vMix's for multi-screen shows), and their users set it by hand. Nida sets
//! the control window's screen itself, for as long as it runs: an app-only
//! display configuration, which macOS undoes when Nida quits — or crashes.
//! Output screens are never switched: one feeding an LED processor or a TV
//! goes black for a moment while it changes mode, on air — so neither is the
//! control window's screen while an output window is on it too. Only the
//! screen set is given back its own mode: giving back every screen's saved
//! mode would undo what another app had set on an output screen.

use std::time::{Duration, Instant};

/// How often the control window's screen is looked at again: it may have
/// been moved to another screen, or the show's rate changed.
const EVERY: Duration = Duration::from_secs(2);

/// How long a screen that could not be set is left alone before it is
/// tried again: its modes may change — another resolution, the lid opened.
const TRY_AGAIN: Duration = Duration::from_secs(60);

/// How long after the screen held is set again before it may be set again:
/// macOS giving it back over and over is not answered every look.
const AGAIN_AFTER: Duration = Duration::from_secs(10);

/// A screen mode: its size in points and in pixels, and its refresh, Hz.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    pub width: usize,
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub refresh: f64,
}

/// Of `modes`, the one to set a screen showing `current` to for a show at
/// `fps`: the same size and sharpness, refreshing at exactly the show's
/// rate. None when the screen already does, the show is uncapped, or the
/// screen offers no such mode. (59.94 is not 60: a frame would be shown twice
/// every 17 seconds.)
pub fn mode_for(current: &Mode, modes: &[Mode], fps: u32) -> Option<usize> {
    if fps == 0 {
        return None;
    }
    let fps = f64::from(fps);
    if (current.refresh - fps).abs() < 0.01 {
        return None;
    }
    modes.iter().position(|m| {
        (m.width, m.height, m.pixel_width, m.pixel_height)
            == (current.width, current.height, current.pixel_width, current.pixel_height)
            && (m.refresh - fps).abs() < 0.01
    })
}

/// What [`ScreenRate::keep`] did, for the log and the operator.
#[derive(Debug, Clone, PartialEq)]
pub enum Kept {
    /// The screen was set to the show's rate, from the refresh it had.
    Set { display: u32, from: f64, to: f64 },
    /// The screen held was found off the show's rate — at `found` — and set
    /// to it again: macOS gives screens back their own mode as the screens
    /// change (one plugged in or out).
    SetAgain { display: u32, found: f64, to: f64 },
    /// The screen was given back its own mode — at `rate` Hz, when it can
    /// be read.
    GivenBack { display: u32, rate: Option<f64> },
}

/// Sets a screen to a rate: the refresh it had and the one set, or none.
type SetRate = fn(u32, u32) -> Result<Option<(f64, f64)>, String>;

/// Gives a screen set back its own mode: whether it did.
type GiveBack = fn(u32) -> bool;

/// A screen's refresh now, Hz, if it can be read.
type CurrentRate = fn(u32) -> Option<f64>;

/// Keeps the control window's screen at the show's rate (see the module
/// doc), looking again every couple of seconds.
#[derive(Debug)]
pub struct ScreenRate {
    /// The screen set, and the rate it was set to.
    held: Option<(u32, u32)>,
    /// Screens and rates tried that could not be set, and when: not tried
    /// again for [`TRY_AGAIN`].
    tried: Vec<(u32, u32, Instant)>,
    checked: Option<Instant>,
    /// When the screen held was last set again (see [`AGAIN_AFTER`]).
    again_at: Option<Instant>,
    /// What sets a screen's rate, and what gives a screen back its own
    /// mode: macOS's, but for tests, which must not change the screens of
    /// the machine they run on.
    set_rate: SetRate,
    give_back: GiveBack,
    current_rate: CurrentRate,
}

impl Default for ScreenRate {
    fn default() -> Self {
        Self {
            held: None,
            tried: Vec::new(),
            checked: None,
            again_at: None,
            set_rate,
            give_back,
            current_rate,
        }
    }
}

impl ScreenRate {
    /// With the control window on the screen `display` says, output windows
    /// on the screens `outputs` says — as they are now; `None` while one's
    /// screen is not known — and the show at `fps`: set the control window's
    /// screen to the show's rate (`on`) unless an output window is on it too,
    /// or give it back its own mode. Looks every couple of seconds at most —
    /// `display` and `outputs` are asked only then — and changes nothing
    /// while an output window's screen is not known; what it did, if
    /// anything.
    pub fn keep(
        &mut self,
        display: impl FnOnce() -> Option<u32>,
        outputs: impl FnOnce() -> Option<Vec<u32>>,
        fps: u32,
        on: bool,
    ) -> Option<Kept> {
        let now = Instant::now();
        if self.checked.is_some_and(|at| now < at + EVERY) {
            return None;
        }
        self.checked = Some(now);
        self.tried.retain(|&(_, _, at)| now < at + TRY_AGAIN);
        let asked = on && fps > 0;
        if !asked && self.held.is_none() {
            return None;
        }
        // Where every output window is now: nothing is changed while one's
        // screen is not known.
        let outputs = outputs()?;
        let wanted = asked
            .then(display)
            .flatten()
            .filter(|d| !outputs.contains(d))
            .map(|d| (d, fps));
        if wanted == self.held {
            // Held — unless macOS dropped it: a screen plugged in or out
            // gives every screen back its own mode, and the control window's
            // was left at its faster rate for the rest of the show (a laptop,
            // 2026-10-06). Then it is set again.
            let (display, fps) = self.held?;
            let refused = self.tried.iter().any(|&(d, f, _)| (d, f) == (display, fps));
            if refused || self.again_at.is_some_and(|at| now < at + AGAIN_AFTER) {
                return None;
            }
            let found = (self.current_rate)(display)?;
            if (found - f64::from(fps)).abs() < 0.01 {
                return None;
            }
            self.again_at = Some(now);
            return match (self.set_rate)(display, fps) {
                Ok(Some((_, to))) => Some(Kept::SetAgain { display, found, to }),
                Ok(None) => {
                    self.tried.push((display, fps, now));
                    None
                }
                Err(e) => {
                    log::warn!("The control window's screen could not be set to {fps} Hz again: {e}");
                    self.tried.push((display, fps, now));
                    None
                }
            };
        }
        // Another screen or rate, or none at all: the one set first goes
        // back to its own mode — tried again at the next look if that was
        // refused, and nothing else set meanwhile. Not while an output window
        // is on it: giving it back would change the mode of a screen showing
        // an output, on air; it waits at the show's rate until the output
        // has gone.
        // A screen given back is said so even when another rate is wanted
        // next, unless setting that one says more: given back, it may be at
        // that rate already, and the change went unsaid (60 → 50 fps on a
        // laptop whose own rate was 50 Hz then, 2026-10-06).
        let mut given_back = None;
        if let Some((held, _)) = self.held {
            if outputs.contains(&held) || !(self.give_back)(held) {
                return None;
            }
            self.held = None;
            given_back = Some(Kept::GivenBack {
                display: held,
                // macOS says 0 for a refresh it cannot tell.
                rate: (self.current_rate)(held).filter(|hz| hz.is_finite() && *hz > 0.0),
            });
        }
        let Some((display, fps)) = wanted else {
            return given_back;
        };
        if self.tried.iter().any(|&(d, f, _)| (d, f) == (display, fps)) {
            return given_back;
        }
        match (self.set_rate)(display, fps) {
            Ok(Some((from, to))) => {
                self.held = Some((display, fps));
                Some(Kept::Set { display, from, to })
            }
            Ok(None) => {
                // Nothing to give back later. A screen with no mode at the
                // show's rate is left alone a while; one already at it — left
                // so by a Nida just quit, say — is looked at again next time,
                // for macOS may give it back its own mode.
                let already = (self.current_rate)(display)
                    .is_some_and(|at| (at - f64::from(fps)).abs() < 0.01);
                if !already {
                    self.tried.push((display, fps, now));
                }
                given_back
            }
            Err(e) => {
                log::warn!("The control window's screen could not be set to {fps} Hz: {e}");
                self.tried.push((display, fps, now));
                given_back
            }
        }
    }
}

/// Set `display` to its mode at `fps` for as long as Nida runs: the refresh
/// it had and the one set, or none when there is nothing to change.
#[cfg(target_os = "macos")]
fn set_rate(display: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
    use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFString, CFType};
    use objc2_core_graphics::{
        kCGDisplayShowDuplicateLowResolutionModes, CGBeginDisplayConfiguration,
        CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayWithDisplayMode,
        CGConfigureOption, CGDisplayConfigRef, CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode,
        CGDisplayMode,
    };

    let describe = |mode: &CGDisplayMode| Mode {
        width: CGDisplayMode::width(Some(mode)),
        height: CGDisplayMode::height(Some(mode)),
        pixel_width: CGDisplayMode::pixel_width(Some(mode)),
        pixel_height: CGDisplayMode::pixel_height(Some(mode)),
        refresh: CGDisplayMode::refresh_rate(Some(mode)),
    };
    let own = CGDisplayCopyDisplayMode(display).ok_or("no current mode")?;
    let current = describe(&own);
    // Retina modes are listed as duplicates of their low-resolution twins:
    // asked for, so the screen keeps its sharpness.
    // SAFETY: CoreGraphics' own key, with the boolean it takes.
    let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let value: &CFType = CFBoolean::new(true).as_ref();
    let options = CFDictionary::from_slices(&[key], &[value]);
    // SAFETY: a display id and a dictionary of the options CoreGraphics takes.
    let modes = unsafe { CGDisplayCopyAllDisplayModes(display, Some(options.as_opaque())) }
        .ok_or("no modes")?;
    // SAFETY: CoreGraphics lists display modes.
    let modes: &CFArray<CGDisplayMode> = unsafe { modes.cast_unchecked() };
    let listed: Vec<_> = modes.iter().collect();
    let described: Vec<Mode> = listed.iter().map(|m| describe(m)).collect();
    let Some(chosen) = mode_for(&current, &described, fps) else {
        return Ok(None);
    };
    let mut config: CGDisplayConfigRef = std::ptr::null_mut();
    // SAFETY: `config` is a valid place for the configuration begun.
    if unsafe { CGBeginDisplayConfiguration(&raw mut config) }.0 != 0 {
        return Err("no display configuration".into());
    }
    // SAFETY: the configuration just begun, a display id and one of its own
    // modes.
    let configured =
        unsafe { CGConfigureDisplayWithDisplayMode(config, display, Some(&listed[chosen]), None) };
    if configured.0 != 0 {
        // SAFETY: the configuration begun above, not completed.
        unsafe { CGCancelDisplayConfiguration(config) };
        return Err(format!("the mode was refused ({})", configured.0));
    }
    // For this app only: macOS undoes it when Nida quits, or crashes.
    // SAFETY: the configuration begun above.
    let completed = unsafe { CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly) };
    if completed.0 != 0 {
        return Err(format!("the change was refused ({})", completed.0));
    }
    OWN_MODES.with(|modes| {
        modes.borrow_mut().entry(display).or_insert(own);
    });
    Ok(Some((current.refresh, described[chosen].refresh)))
}

#[cfg(target_os = "macos")]
thread_local! {
    /// Each screen Nida set, and its own mode from before: what it is given
    /// back. On the main thread, where the screens are kept.
    static OWN_MODES: std::cell::RefCell<
        std::collections::HashMap<u32, objc2_core_foundation::CFRetained<objc2_core_graphics::CGDisplayMode>>,
    > = std::cell::RefCell::default();
}

#[cfg(not(target_os = "macos"))]
// Match the fallible platform backend while reporting that no rate was changed.
#[allow(clippy::unnecessary_wraps)]
fn set_rate(_display: u32, _fps: u32) -> Result<Option<(f64, f64)>, String> {
    Ok(None)
}

/// `display`'s refresh now, Hz.
#[cfg(target_os = "macos")]
fn current_rate(display: u32) -> Option<f64> {
    use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode};
    let mode = CGDisplayCopyDisplayMode(display)?;
    Some(CGDisplayMode::refresh_rate(Some(&mode)))
}

#[cfg(not(target_os = "macos"))]
fn current_rate(_display: u32) -> Option<f64> {
    None
}

/// Give `display` back its own mode, from before Nida set it — that screen
/// only, for this app only, as it was set. Whether it was: refused, the mode
/// is kept for the next try (a later `set_rate` keeps the one remembered,
/// never taking Nida's own for the screen's).
#[cfg(target_os = "macos")]
fn give_back(display: u32) -> bool {
    use objc2_core_graphics::{
        CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
        CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDisplayConfigRef,
    };
    let Some(own) = OWN_MODES.with(|modes| modes.borrow().get(&display).cloned()) else {
        return true;
    };
    let mut config: CGDisplayConfigRef = std::ptr::null_mut();
    // SAFETY: `config` is a valid place for the configuration begun.
    if unsafe { CGBeginDisplayConfiguration(&raw mut config) }.0 != 0 {
        log::warn!("Screen {display} not given back its own mode: no display configuration");
        return false;
    }
    // SAFETY: the configuration just begun, a display id and the mode it had.
    let configured = unsafe { CGConfigureDisplayWithDisplayMode(config, display, Some(&own), None) };
    if configured.0 != 0 {
        // SAFETY: the configuration begun above, not completed.
        unsafe { CGCancelDisplayConfiguration(config) };
        log::warn!("Screen {display} not given back its own mode ({})", configured.0);
        return false;
    }
    // SAFETY: the configuration begun above.
    let completed = unsafe { CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly) };
    if completed.0 != 0 {
        log::warn!("Screen {display} not given back its own mode ({})", completed.0);
        return false;
    }
    OWN_MODES.with(|modes| modes.borrow_mut().remove(&display));
    true
}

#[cfg(not(target_os = "macos"))]
fn give_back(_display: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{mode_for, Kept, Mode, ScreenRate};
    use std::sync::atomic::{AtomicU32, Ordering};

    static SET: AtomicU32 = AtomicU32::new(0);
    static GIVEN_BACK: AtomicU32 = AtomicU32::new(0);
    /// The screen given back last, plus one (0: none yet).
    static GIVEN_BACK_SCREEN: AtomicU32 = AtomicU32::new(0);

    // The signature `ScreenRate` takes.
    #[allow(clippy::unnecessary_wraps)]
    fn fake_set(display: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
        SET.fetch_add(1, Ordering::SeqCst);
        // Display 9 has no mode at the show's rate.
        Ok((display != 9).then_some((120.0, f64::from(fps))))
    }

    fn fake_give_back(display: u32) -> bool {
        GIVEN_BACK.fetch_add(1, Ordering::SeqCst);
        GIVEN_BACK_SCREEN.store(display + 1, Ordering::SeqCst);
        true
    }

    /// The control window's screen is set once, left alone while nothing
    /// changes, given back when the setting is turned off, and when the
    /// window moves to another screen that one is set instead — and a
    /// screen with no mode at the show's rate is not tried again.
    #[test]
    fn the_control_screen_is_set_once_and_given_back_when_turned_off() {
        let mut rate = ScreenRate {
            set_rate: fake_set,
            give_back: fake_give_back,
            // Not this machine's screens: their rate is not known.
            current_rate: |_| None,
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate, display: Option<u32>, on: bool| {
            rate.checked = None;
            rate.keep(|| display, || Some(Vec::new()), 60, on)
        };
        assert_eq!(
            look(&mut rate, Some(1), true),
            Some(Kept::Set { display: 1, from: 120.0, to: 60.0 })
        );
        assert_eq!(look(&mut rate, Some(1), true), None, "left alone");
        assert_eq!(SET.load(Ordering::SeqCst), 1);
        assert_eq!(
            rate.keep(|| panic!("asked within two seconds"), || Some(Vec::new()), 60, true),
            None
        );

        assert_eq!(
            look(&mut rate, Some(1), false),
            Some(Kept::GivenBack { display: 1, rate: None })
        );
        assert_eq!(GIVEN_BACK.load(Ordering::SeqCst), 1);
        assert_eq!(look(&mut rate, Some(1), false), None);

        assert!(matches!(look(&mut rate, Some(1), true), Some(Kept::Set { .. })));
        assert!(matches!(
            look(&mut rate, Some(2), true),
            Some(Kept::Set { display: 2, .. })
        ));
        assert_eq!(GIVEN_BACK.load(Ordering::SeqCst), 2, "the first given back");
        assert_eq!(GIVEN_BACK_SCREEN.load(Ordering::SeqCst), 1 + 1, "screen 1 only");

        assert_eq!(
            look(&mut rate, Some(9), true),
            Some(Kept::GivenBack { display: 2, rate: None }),
            "screen 2 given back; 9 has no mode"
        );
        let tries = SET.load(Ordering::SeqCst);
        assert_eq!(look(&mut rate, Some(9), true), None);
        assert_eq!(SET.load(Ordering::SeqCst), tries, "not tried again");
        // A minute later, it may have a mode for the rate.
        for tried in &mut rate.tried {
            tried.2 -= super::TRY_AGAIN;
        }
        assert_eq!(look(&mut rate, Some(9), true), None);
        assert_eq!(SET.load(Ordering::SeqCst), tries + 1, "tried again after a minute");
    }

    /// Held at 60 and then asked for 50, a screen whose own rate is 50 is at
    /// it once given back: nothing more is set, and the change is said.
    #[test]
    fn a_screen_given_back_at_the_new_rate_says_so() {
        let mut rate = ScreenRate {
            set_rate: |_, fps| Ok::<_, String>((fps != 50).then_some((50.0, f64::from(fps)))),
            give_back: |_| true,
            current_rate: |_| Some(50.0),
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate, fps: u32| {
            rate.checked = None;
            rate.keep(|| Some(1), || Some(Vec::new()), fps, true)
        };
        assert_eq!(
            look(&mut rate, 60),
            Some(Kept::Set { display: 1, from: 50.0, to: 60.0 })
        );
        assert_eq!(
            look(&mut rate, 50),
            Some(Kept::GivenBack { display: 1, rate: Some(50.0) })
        );
        assert_eq!(look(&mut rate, 50), None, "then left alone");
    }

    /// A refresh macOS cannot tell — it says 0 — is left out, not said as
    /// "0 Hz".
    #[test]
    fn a_refresh_that_cannot_be_read_is_not_said() {
        let mut rate = ScreenRate {
            set_rate: |_, fps| Ok::<_, String>(Some((60.0, f64::from(fps)))),
            give_back: |_| true,
            current_rate: |_| Some(0.0),
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate, on: bool| {
            rate.checked = None;
            rate.keep(|| Some(1), || Some(Vec::new()), 60, on)
        };
        assert!(matches!(look(&mut rate, true), Some(Kept::Set { .. })));
        assert_eq!(
            look(&mut rate, false),
            Some(Kept::GivenBack { display: 1, rate: None })
        );
    }

    /// A control window on a screen an output window is on too leaves that
    /// screen as it is — and gives it back its own mode if it had set it,
    /// an output window having come onto it.
    #[test]
    fn a_screen_with_an_output_window_on_it_is_never_switched() {
        static SET_HERE: AtomicU32 = AtomicU32::new(0);
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn set_here(_: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
            SET_HERE.fetch_add(1, Ordering::SeqCst);
            Ok(Some((120.0, f64::from(fps))))
        }
        static BACK_HERE: AtomicU32 = AtomicU32::new(0);
        fn back_here(display: u32) -> bool {
            BACK_HERE.store(display + 1, Ordering::SeqCst);
            true
        }
        let mut rate = ScreenRate {
            set_rate: set_here,
            give_back: back_here,
            // Not this machine's screens: their rate is not known.
            current_rate: |_| None,
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate, outputs: Option<Vec<u32>>| {
            rate.checked = None;
            rate.keep(|| Some(5), move || outputs, 60, true)
        };
        assert_eq!(look(&mut rate, Some(vec![5, 6])), None, "an output on it");
        assert_eq!(look(&mut rate, None), None, "an output's screen not known");
        assert_eq!(SET_HERE.load(Ordering::SeqCst), 0);
        assert!(matches!(look(&mut rate, Some(vec![6])), Some(Kept::Set { display: 5, .. })));
        assert_eq!(look(&mut rate, None), None, "not known: left as it is");
        assert_eq!(look(&mut rate, Some(vec![6, 5])), None, "an output came: not changed under it");
        assert_eq!(BACK_HERE.load(Ordering::SeqCst), 0, "nothing given back under an output");
        // Turned off, it still waits for the output to go.
        rate.checked = None;
        assert_eq!(rate.keep(|| Some(5), || Some(vec![5]), 60, false), None);
        assert_eq!(BACK_HERE.load(Ordering::SeqCst), 0);
        rate.checked = None;
        assert!(matches!(
            rate.keep(|| Some(5), || Some(vec![6]), 60, false),
            Some(Kept::GivenBack { display: 5, .. })
        ));
        assert_eq!(BACK_HERE.load(Ordering::SeqCst), 5 + 1, "given back once the output left");
    }

    /// A screen held at the show's rate that macOS gives back its own mode —
    /// a screen plugged in or out — is set again at the next look; while it
    /// stays at the rate, it is left alone.
    #[test]
    fn a_screen_given_back_by_macos_is_set_again() {
        static RATE_NOW: AtomicU32 = AtomicU32::new(120);
        static SETS: AtomicU32 = AtomicU32::new(0);
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn set_now(_: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
            SETS.fetch_add(1, Ordering::SeqCst);
            let from = f64::from(RATE_NOW.swap(fps, Ordering::SeqCst));
            Ok(Some((from, f64::from(fps))))
        }
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn rate_now(_: u32) -> Option<f64> {
            Some(f64::from(RATE_NOW.load(Ordering::SeqCst)))
        }
        let mut rate = ScreenRate {
            set_rate: set_now,
            give_back: fake_give_back,
            current_rate: rate_now,
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate| {
            rate.checked = None;
            rate.keep(|| Some(1), || Some(Vec::new()), 60, true)
        };
        assert!(matches!(look(&mut rate), Some(Kept::Set { display: 1, .. })));
        assert_eq!(look(&mut rate), None, "held, at the rate");
        RATE_NOW.store(120, Ordering::SeqCst);
        assert_eq!(
            look(&mut rate),
            Some(Kept::SetAgain { display: 1, found: 120.0, to: 60.0 })
        );
        assert_eq!(look(&mut rate), None);
        assert_eq!(SETS.load(Ordering::SeqCst), 2);
        // Given back again at once: not set again every look.
        RATE_NOW.store(120, Ordering::SeqCst);
        assert_eq!(look(&mut rate), None, "not again within a few seconds");
        assert_eq!(SETS.load(Ordering::SeqCst), 2);
        rate.again_at = rate
            .again_at
            .and_then(|at| at.checked_sub(super::AGAIN_AFTER));
        assert!(matches!(look(&mut rate), Some(Kept::SetAgain { .. })), "later, yes");
    }

    /// A screen found already at the show's rate — a Nida just quit left it
    /// so — is not set, but looked at again: once macOS gives it back its own
    /// rate, it is set.
    #[test]
    fn a_screen_found_at_the_rate_is_watched_not_shelved() {
        static RATE_THERE: AtomicU32 = AtomicU32::new(60);
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn set_there(_: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
            let from = RATE_THERE.load(Ordering::SeqCst);
            if from == fps {
                return Ok(None);
            }
            RATE_THERE.store(fps, Ordering::SeqCst);
            Ok(Some((f64::from(from), f64::from(fps))))
        }
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn rate_there(_: u32) -> Option<f64> {
            Some(f64::from(RATE_THERE.load(Ordering::SeqCst)))
        }
        let mut rate = ScreenRate {
            set_rate: set_there,
            give_back: fake_give_back,
            current_rate: rate_there,
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate| {
            rate.checked = None;
            rate.keep(|| Some(1), || Some(Vec::new()), 60, true)
        };
        assert_eq!(look(&mut rate), None, "already at the rate");
        assert!(rate.tried.is_empty(), "not put aside for a minute");
        RATE_THERE.store(120, Ordering::SeqCst);
        assert!(matches!(look(&mut rate), Some(Kept::Set { display: 1, .. })));
    }

    /// A screen whose own mode could not be given back is tried again at the
    /// next look, and no other screen is set meanwhile.
    #[test]
    fn a_screen_not_given_back_is_tried_again() {
        static REFUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
        static TRIES: AtomicU32 = AtomicU32::new(0);
        // The signature `ScreenRate` takes.
        #[allow(clippy::unnecessary_wraps)]
        fn set_it(_: u32, fps: u32) -> Result<Option<(f64, f64)>, String> {
            Ok(Some((120.0, f64::from(fps))))
        }
        fn back_maybe(_: u32) -> bool {
            TRIES.fetch_add(1, Ordering::SeqCst);
            !REFUSE.load(Ordering::SeqCst)
        }
        let mut rate = ScreenRate {
            set_rate: set_it,
            give_back: back_maybe,
            // Not this machine's screens: their rate is not known.
            current_rate: |_| None,
            ..ScreenRate::default()
        };
        let look = |rate: &mut ScreenRate, display: u32, on: bool| {
            rate.checked = None;
            rate.keep(|| Some(display), || Some(Vec::new()), 60, on)
        };
        assert!(matches!(look(&mut rate, 1, true), Some(Kept::Set { .. })));
        assert_eq!(look(&mut rate, 1, false), None, "refused");
        assert_eq!(look(&mut rate, 2, true), None, "still holding screen 1: nothing else set");
        assert_eq!(TRIES.load(Ordering::SeqCst), 2);
        REFUSE.store(false, Ordering::SeqCst);
        assert_eq!(
            look(&mut rate, 1, false),
            Some(Kept::GivenBack { display: 1, rate: None })
        );
    }

    fn mode(width: usize, pixel_width: usize, refresh: f64) -> Mode {
        Mode {
            width,
            height: width * 9 / 16,
            pixel_width,
            pixel_height: pixel_width * 9 / 16,
            refresh,
        }
    }

    /// A `ProMotion` screen at 120 Hz with a 60 fps show is set to its 60 Hz
    /// mode of the same size and sharpness — not another size's, nor
    /// 59.94.
    #[test]
    fn a_120_hz_screen_is_set_to_its_60_hz_mode_of_the_same_size() {
        let current = mode(1728, 3456, 120.0);
        let modes = [
            mode(1728, 1728, 60.0),
            mode(1512, 3024, 60.0),
            mode(1728, 3456, 59.94),
            mode(1728, 3456, 120.0),
            mode(1728, 3456, 60.0),
            mode(1728, 3456, 50.0),
        ];
        assert_eq!(mode_for(&current, &modes, 60), Some(4));
        assert_eq!(mode_for(&current, &modes, 50), Some(5));
    }

    /// Nothing changes when the screen already runs at the show's rate,
    /// the show is uncapped, or no mode fits.
    #[test]
    fn nothing_changes_without_a_better_mode() {
        let at_60 = mode(1728, 3456, 60.0);
        let modes = [mode(1728, 3456, 60.0), mode(1728, 3456, 120.0)];
        assert_eq!(mode_for(&at_60, &modes, 60), None, "already there");
        let at_120 = mode(1728, 3456, 120.0);
        assert_eq!(mode_for(&at_120, &modes, 0), None, "uncapped");
        assert_eq!(mode_for(&at_120, &modes, 25), None, "no 25 Hz mode");
        assert_eq!(
            mode_for(&mode(1920, 1920, 59.94), &[mode(1920, 1920, 60.0)], 60),
            Some(0),
            "59.94 to 60"
        );
    }
}
