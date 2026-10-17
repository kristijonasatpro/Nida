//! Frames started on the refresh of the screen that shows the show — the
//! first output's (the TV), else the control window's — rather than on a
//! timer of Nida's own.
//!
//! A timer set for 60 frames a second wakes a millisecond or two off, and
//! loses a little more whenever a frame runs long; measured on a laptop it
//! averaged 57.8–59.2 frames a second into a TV refreshing at exactly 60.000.
//! The TV then shows a frame twice once or twice a second, and anything
//! moving — a slide — visibly hitches. Started by the screen's own refresh,
//! a frame is drawn for every refresh the screen shows (or every other, for
//! a 120 Hz screen and a 60 frames-a-second show).
//!
//! On macOS a display link calls back on each refresh, on a thread of its
//! own; the callback only wakes the event loop, which draws the frame.
//! Elsewhere there is no display clock and the timer stays.

use std::collections::VecDeque;
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::AtomicI64;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Callbacks kept for the performance figures: five seconds at 120 Hz.
#[cfg(any(target_os = "macos", test))]
const TICKS_KEPT: usize = 600;

/// One display-link callback: when it came, and the refresh it is for —
/// the time macOS stamps it with — both in nanoseconds after `origin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tick {
    pub at: u64,
    pub shown: Option<u64>,
}

/// What the display link shares with the frame loop.
struct Shared {
    /// Callbacks seen.
    ticks: AtomicU64,
    /// Frames asked for.
    wakes: AtomicU64,
    /// Every how many refreshes a frame is drawn.
    #[cfg(any(target_os = "macos", test))]
    every: u64,
    /// One refresh, in nanoseconds.
    #[cfg(any(target_os = "macos", test))]
    period: u64,
    /// When the callback that last asked for a frame came, in nanoseconds
    /// after `origin`.
    last: AtomicU64,
    /// The refresh it was for (0: not known).
    last_shown: AtomicU64,
    /// Frames asked for by the refresh each callback is stamped for, not by
    /// counting callbacks — which miscounts when one comes late or two come
    /// together.
    by_time: AtomicBool,
    /// The last frame slot (refresh ÷ `every`) asked for, by time.
    #[cfg(any(target_os = "macos", test))]
    last_slot: AtomicI64,
    recent: Mutex<VecDeque<Tick>>,
    origin: Instant,
    /// Wakes the event loop to draw a frame.
    #[cfg(any(target_os = "macos", test))]
    wake: Box<dyn Fn() + Send + Sync>,
}

#[cfg(any(target_os = "macos", test))]
impl Shared {
    fn new(every: u32, refresh_hz: f64, wake: Box<dyn Fn() + Send + Sync>) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let period = if refresh_hz > 0.0 {
            (1e9 / refresh_hz) as u64
        } else {
            16_666_667
        };
        Self {
            ticks: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
            every: u64::from(every.max(1)),
            period: period.max(1),
            last: AtomicU64::new(0),
            last_shown: AtomicU64::new(0),
            by_time: AtomicBool::new(false),
            last_slot: AtomicI64::new(i64::MIN),
            recent: Mutex::new(VecDeque::with_capacity(TICKS_KEPT)),
            origin: Instant::now(),
            wake,
        }
    }

    fn now(&self) -> u64 {
        #[allow(clippy::cast_possible_truncation)]
        let at = self.origin.elapsed().as_nanos() as u64;
        at.max(1)
    }

    /// A callback, for the refresh at `shown` (nanoseconds after `origin`)
    /// when macOS said: every `every`th refresh asks for a frame.
    fn tick(&self, shown: Option<u64>) {
        let at = self.now();
        let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() >= TICKS_KEPT {
                recent.pop_front();
            }
            recent.push_back(Tick { at, shown });
        }
        let due = match shown.filter(|_| self.by_time.load(Ordering::Relaxed)) {
            Some(shown) => {
                #[allow(clippy::cast_possible_wrap)]
                let slot = ((shown + self.period / 2) / self.period / self.every) as i64;
                self.last_slot.fetch_max(slot, Ordering::Relaxed) < slot
            }
            None => n.is_multiple_of(self.every),
        };
        if due {
            self.last.store(at, Ordering::Relaxed);
            self.last_shown.store(shown.unwrap_or(0), Ordering::Relaxed);
            self.wakes.fetch_add(1, Ordering::Relaxed);
            (self.wake)();
        }
    }
}

/// How long without a refresh before the display clock is thought stopped
/// (the screen asleep or gone) and the timer takes over.
const STOPPED: Duration = Duration::from_millis(100);

/// A screen's refresh, calling for frames.
pub(super) struct DisplayClock {
    display: u32,
    every: u32,
    refresh_hz: f64,
    shared: Arc<Shared>,
    /// Held for its drop, which stops the link.
    #[cfg(target_os = "macos")]
    _link: mac::Link,
}

impl DisplayClock {
    /// Ask for a frame every `every` refreshes of display `display` (a
    /// `CGDirectDisplayID`), by calling `wake`. `None` where there is no
    /// display clock, or it would not start.
    pub fn start(
        display: u32,
        every: u32,
        refresh_hz: f64,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let shared = Arc::new(Shared::new(every, refresh_hz, Box::new(wake)));
            let link = mac::Link::start(display, Arc::clone(&shared))?;
            Some(Self {
                display,
                every: every.max(1),
                refresh_hz,
                shared,
                _link: link,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (display, every, refresh_hz, wake);
            None
        }
    }

    pub fn display(&self) -> u32 {
        self.display
    }

    pub fn every(&self) -> u32 {
        self.every
    }

    /// Whether this clock is the one wanted: the screen `display`, a frame
    /// every `every` refreshes, at `hz` (see [`same_clock`]).
    pub fn is(&self, display: u32, every: u32, hz: f64) -> bool {
        same_clock((self.display, self.every, self.refresh_hz), (display, every, hz))
    }

    /// Whether refreshes are arriving: once one has, and none is overdue.
    pub fn running(&self) -> bool {
        let last = self.shared.last.load(Ordering::Relaxed);
        last != 0
            && self
                .shared
                .origin
                .elapsed()
                .saturating_sub(Duration::from_nanos(last))
                < STOPPED
    }

    /// When the refresh that last called for a frame came — by the time
    /// macOS stamped it with, when frames are asked for by that.
    pub fn last_tick(&self) -> Option<Instant> {
        let shown = self.shared.last_shown.load(Ordering::Relaxed);
        let last = if self.by_time() && shown != 0 {
            shown
        } else {
            self.shared.last.load(Ordering::Relaxed)
        };
        (last != 0).then(|| self.shared.origin + Duration::from_nanos(last))
    }

    /// When the callback that last asked for a frame came.
    pub fn last_arrival(&self) -> Option<Instant> {
        let last = self.shared.last.load(Ordering::Relaxed);
        (last != 0).then(|| self.shared.origin + Duration::from_nanos(last))
    }

    /// Ask for frames by the refresh each callback is stamped for, rather
    /// than by counting callbacks.
    pub fn set_by_time(&self, on: bool) {
        self.shared.by_time.store(on, Ordering::Relaxed);
    }

    pub fn by_time(&self) -> bool {
        self.shared.by_time.load(Ordering::Relaxed)
    }

    /// Callbacks seen, and frames asked for.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.shared.ticks.load(Ordering::Relaxed),
            self.shared.wakes.load(Ordering::Relaxed),
        )
    }

    /// The last callbacks, oldest first.
    pub fn recent(&self) -> Vec<Tick> {
        self.shared
            .recent
            .lock()
            .map(|r| r.iter().copied().collect())
            .unwrap_or_default()
    }

    /// How long since the refresh that last called for a frame.
    pub fn since_tick(&self) -> Option<Duration> {
        let last = self.shared.last.load(Ordering::Relaxed);
        (last != 0).then(|| {
            self.shared
                .origin
                .elapsed()
                .saturating_sub(Duration::from_nanos(last))
        })
    }

    /// What starts frames, for the performance figures.
    pub fn describe(&self) -> String {
        let every = match self.every {
            1 => "every refresh".to_string(),
            n => format!("every {n} refreshes"),
        };
        format!(
            "display {} at {:.2} Hz, {every}",
            self.display, self.refresh_hz
        )
    }
}

pub(super) use crate::app::monitors::refreshes_per_frame;

/// Whether a clock running on `(display, every, hz)` is the one `wanted`.
/// The screen's rate counts as well as how many refreshes make a frame: a
/// screen gone from 60 to 50 Hz under a 50 fps show is still "every
/// refresh", but a clock left at 60 named the wrong rate and, by time, cut
/// its frames from refreshes 16.7 ms apart (a laptop, 2026-10-06). Within
/// half a percent is the same rate — 59.94 is 60's own wobble, not a change.
pub(super) fn same_clock(running: (u32, u32, f64), wanted: (u32, u32, f64)) -> bool {
    running.0 == wanted.0
        && running.1 == wanted.1
        && (running.2 - wanted.2).abs() <= wanted.2.abs() * 0.005
}

#[cfg(target_os = "macos")]
mod mac {
    //! The display link: `CVDisplayLink`, deprecated for `NSScreen`'s
    //! display link but still working, and callable from any thread.
    #![allow(deprecated)]

    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Arc;

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{kCVReturnSuccess, CVDisplayLink, CVOptionFlags, CVReturn, CVTimeStamp};

    use super::Shared;

    pub(super) struct Link {
        link: CFRetained<CVDisplayLink>,
        /// The `Arc` the callback reads, handed over as a raw pointer.
        shared: *const Shared,
    }

    // The link is only started and stopped from here; the callback touches
    // only `Shared`, which is Sync.
    unsafe impl Send for Link {}

    impl Link {
        pub(super) fn start(display: u32, shared: Arc<Shared>) -> Option<Self> {
            let mut raw: *mut CVDisplayLink = std::ptr::null_mut();
            // SAFETY: `raw` is a valid place for the new link.
            let made =
                unsafe { CVDisplayLink::create_with_cg_display(display, NonNull::from(&mut raw)) };
            if made != kCVReturnSuccess {
                log::warn!("No display link for display {display}: {made}");
                return None;
            }
            // SAFETY: created, retained once, and ours now.
            let link = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
            let shared = Arc::into_raw(shared);
            // SAFETY: `on_refresh` reads `shared` as a `Shared`, which lives
            // until the link is stopped in `drop`.
            let set =
                unsafe { link.set_output_callback(Some(on_refresh), shared.cast_mut().cast()) };
            let started = if set == kCVReturnSuccess {
                link.start()
            } else {
                set
            };
            if started != kCVReturnSuccess {
                log::warn!("Display link for display {display} did not start: {started}");
                // SAFETY: never started, so never read.
                drop(unsafe { Arc::from_raw(shared) });
                return None;
            }
            Some(Self { link, shared })
        }
    }

    impl Drop for Link {
        fn drop(&mut self) {
            // Stopped before its `Shared` goes: `stop` returns once no
            // callback is running.
            self.link.stop();
            // SAFETY: from `Arc::into_raw` in `start`, and no longer read.
            drop(unsafe { Arc::from_raw(self.shared) });
        }
    }

    unsafe extern "C-unwind" fn on_refresh(
        _link: NonNull<CVDisplayLink>,
        _now: NonNull<CVTimeStamp>,
        out: NonNull<CVTimeStamp>,
        _flags: CVOptionFlags,
        _flags_out: NonNull<CVOptionFlags>,
        shared: *mut c_void,
    ) -> CVReturn {
        // SAFETY: the pointer set in `start`, alive until `drop` stops us.
        if let Some(shared) = unsafe { shared.cast::<Shared>().as_ref() } {
            // SAFETY: CoreVideo passes a valid timestamp for the call.
            let out = unsafe { out.as_ref() };
            shared.tick(shown_at(shared, out));
        }
        kCVReturnSuccess
    }

    /// The refresh `out` is for, in nanoseconds after the link's origin:
    /// its host time, which runs on `mach_absolute_time`'s clock.
    fn shown_at(shared: &Shared, out: &CVTimeStamp) -> Option<u64> {
        const HOST_TIME_VALID: u64 = 1 << 1;
        if out.flags & HOST_TIME_VALID == 0 || out.hostTime == 0 {
            return None;
        }
        let (numer, denom) = timebase();
        // SAFETY: no arguments; reads the host clock.
        let host_now = unsafe { libc::mach_absolute_time() };
        let ahead = (i128::from(out.hostTime) - i128::from(host_now)) * i128::from(numer)
            / i128::from(denom.max(1));
        let shown = i128::from(shared.now()) + ahead;
        u64::try_from(shown).ok().filter(|s| *s > 0)
    }

    fn timebase() -> (u32, u32) {
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        *TIMEBASE.get_or_init(|| {
            let mut info = libc::mach_timebase_info { numer: 1, denom: 1 };
            // SAFETY: `info` is a valid place for the answer.
            unsafe { libc::mach_timebase_info(&raw mut info) };
            (info.numer, info.denom)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counting(every: u32) -> (Shared, Arc<AtomicU64>) {
        let woken = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&woken);
        let shared = Shared::new(
            every,
            120.0,
            Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        );
        (shared, woken)
    }

    /// A screen whose rate changes under the clock needs a new clock, even
    /// at the same refreshes a frame; the same rate within its wobble does
    /// not, nor does anything when nothing changed.
    #[test]
    fn a_screen_that_changes_rate_needs_a_new_clock() {
        assert!(same_clock((1, 1, 60.0), (1, 1, 60.0)));
        assert!(same_clock((1, 1, 60.0), (1, 1, 59.94)), "60's own wobble");
        assert!(!same_clock((1, 1, 60.0), (1, 1, 50.0)), "60 → 50 Hz at 50 fps");
        assert!(!same_clock((1, 2, 120.0), (1, 2, 100.0)), "120 → 100 Hz at 50 fps");
        assert!(!same_clock((1, 1, 60.0), (2, 1, 60.0)), "another screen");
        assert!(!same_clock((1, 1, 60.0), (1, 2, 60.0)), "another rate of frames");
    }

    #[test]
    fn every_nth_refresh_wakes_the_loop() {
        let (shared, woken) = counting(2);
        for _ in 0..6 {
            shared.tick(None);
        }
        assert_eq!(woken.load(Ordering::Relaxed), 3);
        assert_eq!(shared.recent.lock().unwrap().len(), 6, "each callback kept");
    }

    /// By the refresh each callback is stamped for, a late callback, or two
    /// arriving together, still ask for one frame per two refreshes: none
    /// missed, none twice.
    #[test]
    fn by_time_a_late_or_doubled_callback_asks_for_one_frame_a_slot() {
        let (shared, woken) = counting(2);
        shared.by_time.store(true, Ordering::Relaxed);
        let refresh = shared.period;
        // Refreshes 1..=8, but the one for refresh 4 comes twice and the
        // one for refresh 6 never comes.
        for r in [1_u64, 2, 3, 4, 4, 5, 7, 8] {
            shared.tick(Some(r * refresh));
        }
        // Slots (refresh ÷ 2): 0, 1, 1, 2, 2, 2, 3, 4 → woken for 0, 1, 2, 3, 4.
        assert_eq!(woken.load(Ordering::Relaxed), 5);
        // Counting callbacks instead, the doubled one shifts every frame
        // after it onto the other refresh.
        let (counted, woken) = counting(2);
        for r in [1_u64, 2, 3, 4, 4, 5, 7, 8] {
            counted.tick(Some(r * refresh));
        }
        assert_eq!(woken.load(Ordering::Relaxed), 4);
    }

    /// A real display link, on this Mac's main display: it calls back about
    /// as often as the display refreshes.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs a display; run by hand"]
    fn the_main_display_calls_for_frames() {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGMainDisplayID() -> u32;
        }
        let woken = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&woken);
        // SAFETY: no arguments, no state.
        let main = unsafe { CGMainDisplayID() };
        let clock = DisplayClock::start(main, 1, 60.0, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        })
        .expect("a display link");
        std::thread::sleep(Duration::from_millis(500));
        assert!(clock.running());
        let n = woken.load(Ordering::Relaxed);
        assert!((20..=70).contains(&n), "{n} refreshes in half a second");
    }
}
