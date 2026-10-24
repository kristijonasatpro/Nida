//! Night Shift kept off while an output is on a screen.
//!
//! Night Shift warms every screen of the Mac in the evening — its outputs
//! too: an LED wall, a TV, a timer on HDMI turned yellowish with it (an
//! event, 2026-10-22). macOS warms all its screens alike and lets no app
//! leave one out; what an app can do is what Control Center does — turn it
//! off. So while an output window is on a screen Nida keeps Night Shift off,
//! this Mac's own screen going neutral with the rest, and when none is any
//! more, or Nida quits, gives it back: on again if Nida turned it off and it
//! is due on by then — within its schedule's hours, or between sunset and
//! sunrise; with no schedule, at night, as it was turned on by hand. When it
//! is not due on it stays off until its schedule next turns it on; so it
//! does if Nida ends in a crash. An output on a Blackmagic device is not
//! warmed, and does not count. Night Shift is macOS's own setting, read and
//! set through `CoreBrightness`, the private framework behind it, as Shifty
//! and the `nightlight` tool do; nothing of it is asked while no output is
//! on a screen and Nida holds nothing.

use std::time::{Duration, Instant};

/// How often Night Shift is looked at while an output is on a screen, or
/// Nida holds it off: its schedule may turn it on mid-show, at sunset.
const EVERY: Duration = Duration::from_secs(5);

/// What turns Night Shift on and off by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Nothing: it is turned on by hand, until the next day.
    ByHand,
    /// From sunset to sunrise.
    Sun,
    /// Every day from one time to another, as (hour, minute).
    Hours { from: (u8, u8), to: (u8, u8) },
}

/// Night Shift as macOS has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Warming the screens now.
    pub on: bool,
    pub schedule: Schedule,
}

/// What [`NightShift::keep`] did, for the log and the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// Turned off, an output being on a screen; `first`, the first time
    /// since Nida started — the one said to the operator.
    TurnedOff { first: bool },
    /// Given back: `on` again — due on now, or turned on meanwhile — or
    /// left off till its schedule next turns it on.
    GivenBack { on: bool },
}

/// Whether Night Shift is due on at `minute` of the day, local time: within
/// its hours; between sunset and sunrise (`night`, as macOS reckons it);
/// with no schedule at night — it was on, by hand, when Nida turned it off —
/// or when the night cannot be told.
pub fn due_on(schedule: Schedule, minute: u32, night: Option<bool>) -> bool {
    match schedule {
        Schedule::Hours { from, to } => {
            let at = |(hour, min): (u8, u8)| u32::from(hour) * 60 + u32::from(min);
            let (from, to) = (at(from), at(to));
            if from <= to {
                (from..to).contains(&minute)
            } else {
                minute >= from || minute < to
            }
        }
        Schedule::Sun => night == Some(true),
        Schedule::ByHand => night != Some(false),
    }
}

/// Reads Night Shift; `None` when it cannot be.
type ReadStatus = fn() -> Option<Status>;
/// Turns Night Shift on or off: whether it did.
type SetOn = fn(bool) -> bool;
/// Whether it is night by the sun, as macOS reckons it; `None` when it
/// cannot tell.
type IsNight = fn() -> Option<bool>;
/// Minutes since midnight, local time.
type MinuteOfDay = fn() -> u32;

/// Keeps Night Shift off while an output is on a screen (see the module
/// doc), looking every few seconds.
#[derive(Debug)]
pub struct NightShift {
    /// Nida turned it off and has not given it back.
    held: bool,
    /// It has been turned off since Nida started.
    turned_off: bool,
    /// Turning it off or back on failed, and the log said so.
    failed: bool,
    checked: Option<Instant>,
    /// macOS's, but for tests, which must not change the Mac they run on.
    status: ReadStatus,
    set_on: SetOn,
    is_night: IsNight,
    minute_of_day: MinuteOfDay,
}

impl Default for NightShift {
    fn default() -> Self {
        Self {
            held: false,
            turned_off: false,
            failed: false,
            checked: None,
            // Unit tests never reach this Mac's own Night Shift: the runner's
            // keeper there reads none (its own tests use fakes).
            status: if cfg!(test) {
                no_status
            } else {
                platform::status
            },
            set_on: platform::set_on,
            is_night: platform::is_night,
            minute_of_day,
        }
    }
}

impl NightShift {
    /// With an output window on a screen or not (`outputs`), and keeping
    /// Night Shift off for them asked or not (`on`): turn it off, or give it
    /// back. Looks every few seconds at most; asks macOS nothing while no
    /// output is on a screen and Nida holds nothing. What it did, if
    /// anything.
    pub fn keep(&mut self, outputs: bool, on: bool) -> Option<Kept> {
        let wanted = on && outputs;
        if !wanted && !self.held {
            return None;
        }
        let now = Instant::now();
        if self.checked.is_some_and(|at| now < at + EVERY) {
            return None;
        }
        self.checked = Some(now);
        let status = (self.status)()?;
        if !wanted {
            return self.give_back(status);
        }
        if !status.on {
            return None;
        }
        if !(self.set_on)(false) {
            if !std::mem::replace(&mut self.failed, true) {
                log::warn!("Night Shift could not be turned off for the outputs");
            }
            return None;
        }
        self.held = true;
        let first = !std::mem::replace(&mut self.turned_off, true);
        Some(Kept::TurnedOff { first })
    }

    /// Give Night Shift back now, if Nida holds it off: as Nida quits.
    pub fn give_back_now(&mut self) -> Option<Kept> {
        if !self.held {
            return None;
        }
        let status = (self.status)()?;
        self.give_back(status)
    }

    /// Give Night Shift back: on again if it is due on now. Turning it on
    /// refused, it is still held — tried again at the next look — and
    /// nothing is given back yet.
    fn give_back(&mut self, status: Status) -> Option<Kept> {
        let on = status.on || due_on(status.schedule, (self.minute_of_day)(), (self.is_night)());
        if on && !status.on && !(self.set_on)(true) {
            if !std::mem::replace(&mut self.failed, true) {
                log::warn!("Night Shift could not be turned back on: trying again");
            }
            return None;
        }
        self.held = false;
        Some(Kept::GivenBack { on })
    }
}

fn no_status() -> Option<Status> {
    None
}

fn minute_of_day() -> u32 {
    use chrono::Timelike as _;
    let now = chrono::Local::now();
    now.hour() * 60 + now.minute()
}

#[cfg(target_os = "macos")]
mod platform {
    //! `CoreBrightness`: `CBBlueLightClient` for Night Shift itself,
    //! `BrightnessSystemClient` for the sun's times, both loaded at run
    //! time — a macOS without them leaves Night Shift alone.

    use super::{Schedule, Status};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;
    use std::ffi::CStr;
    use std::sync::OnceLock;

    const CORE_BRIGHTNESS: &str =
        "/System/Library/PrivateFrameworks/CoreBrightness.framework/CoreBrightness";

    /// Whether `CoreBrightness` is loaded: once, for the life of the process.
    fn loaded() -> bool {
        static LIB: OnceLock<Option<libloading::Library>> = OnceLock::new();
        LIB.get_or_init(
            || match unsafe { libloading::Library::new(CORE_BRIGHTNESS) } {
                Ok(lib) => Some(lib),
                Err(e) => {
                    log::warn!("Night Shift: CoreBrightness could not be loaded: {e}");
                    None
                }
            },
        )
        .is_some()
    }

    fn new_object(class: &CStr) -> Option<Retained<AnyObject>> {
        if !loaded() {
            return None;
        }
        let class = AnyClass::get(class)?;
        unsafe { msg_send![class, new] }
    }

    thread_local! {
        static BLUE_LIGHT: Option<Retained<AnyObject>> = new_object(c"CBBlueLightClient");
        static SUN: Option<Retained<AnyObject>> = new_object(c"BrightnessSystemClient");
    }

    fn responds(object: &AnyObject, selector: Sel) -> bool {
        unsafe { msg_send![object, respondsToSelector: selector] }
    }

    #[repr(C)]
    #[derive(Default)]
    struct Time {
        hour: i32,
        minute: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Hours {
        from: Time,
        to: Time,
    }

    /// `CoreBrightness`'s status: 40 bytes, as macOS 27 fills it in (read
    /// 2026-10-23). Its `BOOL`s are read as bytes.
    #[repr(C)]
    #[derive(Default)]
    struct RawStatus {
        active: u8,
        enabled: u8,
        sun_schedule_permitted: u8,
        mode: i32,
        hours: Hours,
        disable_flags: u64,
        available: u8,
    }

    /// `objc_msgSend` as a method taking one argument, `A`, and answering
    /// `R`: sent with the method's own signature, as its header declares it,
    /// where objc2's checks of a message against the method's encoding would
    /// stand in the way — a C struct, a `CFStringRef` — in a private
    /// framework that may encode a `BOOL` its own way.
    fn send<A, R>(object: &AnyObject, selector: Sel, argument: A) -> R {
        let send: unsafe extern "C-unwind" fn(*const AnyObject, Sel, A) -> R = unsafe {
            std::mem::transmute(objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn())
        };
        unsafe { send(object, selector, argument) }
    }

    pub(super) fn status() -> Option<Status> {
        BLUE_LIGHT.with(|client| {
            let client = client.as_deref()?;
            let selector = sel!(getBlueLightStatus:);
            if !responds(client, selector) {
                return None;
            }
            let mut raw = RawStatus::default();
            let read: Bool = send(client, selector, std::ptr::from_mut(&mut raw));
            if !read.as_bool() || raw.available == 0 {
                return None;
            }
            let time = |t: &Time| {
                (
                    u8::try_from(t.hour).unwrap_or(0),
                    u8::try_from(t.minute).unwrap_or(0),
                )
            };
            let schedule = match raw.mode {
                1 => Schedule::Sun,
                2 => Schedule::Hours {
                    from: time(&raw.hours.from),
                    to: time(&raw.hours.to),
                },
                _ => Schedule::ByHand,
            };
            Some(Status {
                on: raw.enabled != 0,
                schedule,
            })
        })
    }

    pub(super) fn set_on(on: bool) -> bool {
        BLUE_LIGHT.with(|client| {
            let Some(client) = client.as_deref() else {
                return false;
            };
            let selector = sel!(setEnabled:);
            responds(client, selector)
                && send::<Bool, Bool>(client, selector, Bool::new(on)).as_bool()
        })
    }

    pub(super) fn is_night() -> Option<bool> {
        SUN.with(|client| {
            let client = client.as_deref()?;
            let selector = sel!(copyPropertyForKey:);
            if !responds(client, selector) {
                return None;
            }
            let key = NSString::from_str("BlueLightSunSchedule");
            // `copyPropertyForKey:` takes a `CFStringRef` (an `NSString`,
            // toll-free) and answers a dictionary the caller owns.
            let found: *mut AnyObject = send(client, selector, Retained::as_ptr(&key));
            let schedule = unsafe { Retained::from_raw(found) }?;
            if !responds(&schedule, sel!(objectForKey:)) {
                return None;
            }
            let daylight_key = NSString::from_str("isDaylight");
            let daylight: Option<Retained<AnyObject>> =
                unsafe { msg_send![&*schedule, objectForKey: &*daylight_key] };
            let daylight = daylight?;
            if !responds(&daylight, sel!(boolValue)) {
                return None;
            }
            let is_daylight: bool = unsafe { msg_send![&*daylight, boolValue] };
            Some(!is_daylight)
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(super) fn status() -> Option<super::Status> {
        None
    }
    pub(super) fn set_on(_on: bool) -> bool {
        false
    }
    pub(super) fn is_night() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{due_on, Kept, NightShift, Schedule, Status};
    use std::cell::Cell;

    const NINE_PM: u32 = 21 * 60;
    const NOON: u32 = 12 * 60;

    #[test]
    fn due_on_within_its_hours_and_by_the_sun() {
        let evening = Schedule::Hours {
            from: (22, 0),
            to: (7, 0),
        };
        assert!(due_on(evening, 23 * 60, None), "after it starts");
        assert!(
            due_on(evening, 6 * 60 + 59, None),
            "before it ends, next day"
        );
        assert!(!due_on(evening, 7 * 60, None), "at its end");
        assert!(!due_on(evening, NINE_PM, None));
        let afternoon = Schedule::Hours {
            from: (13, 30),
            to: (18, 0),
        };
        assert!(due_on(afternoon, 14 * 60, None));
        assert!(!due_on(afternoon, NOON, None));
        assert!(due_on(Schedule::Sun, NOON, Some(true)), "the sun says");
        assert!(!due_on(Schedule::Sun, NINE_PM, Some(false)));
        assert!(!due_on(Schedule::Sun, NINE_PM, None), "unknown: left off");
        assert!(due_on(Schedule::ByHand, NOON, None), "as it was found");
        assert!(due_on(Schedule::ByHand, NOON, Some(true)));
        assert!(
            !due_on(Schedule::ByHand, NINE_PM, Some(false)),
            "day by now"
        );
    }

    // A fake macOS, per test thread: never this Mac's own Night Shift.
    thread_local! {
        static ON: Cell<bool> = const { Cell::new(true) };
        static SCHEDULE: Cell<Schedule> = const { Cell::new(Schedule::Sun) };
        static NIGHT: Cell<Option<bool>> = const { Cell::new(Some(true)) };
        static ASKED: Cell<u32> = const { Cell::new(0) };
        static SET: Cell<u32> = const { Cell::new(0) };
        static REFUSE: Cell<bool> = const { Cell::new(false) };
    }

    #[allow(clippy::unnecessary_wraps)]
    fn fake_status() -> Option<Status> {
        ASKED.with(|n| n.set(n.get() + 1));
        Some(Status {
            on: ON.with(Cell::get),
            schedule: SCHEDULE.with(Cell::get),
        })
    }

    fn fake_set(on: bool) -> bool {
        SET.with(|n| n.set(n.get() + 1));
        ON.with(|o| o.set(on));
        true
    }

    fn fake() -> NightShift {
        NightShift {
            status: fake_status,
            set_on: fake_set,
            is_night: || NIGHT.with(Cell::get),
            minute_of_day: || NINE_PM,
            ..NightShift::default()
        }
    }

    fn look(night_shift: &mut NightShift, outputs: bool, on: bool) -> Option<Kept> {
        night_shift.checked = None;
        night_shift.keep(outputs, on)
    }

    /// Off while an output is on a screen — again when its schedule turns
    /// it on mid-show — and given back on when none is, at night.
    #[test]
    fn off_while_an_output_is_on_a_screen_and_given_back_at_night() {
        let mut night_shift = fake();
        assert_eq!(look(&mut night_shift, false, true), None);
        assert_eq!(ASKED.with(Cell::get), 0, "nothing asked with no output");

        assert_eq!(
            look(&mut night_shift, true, true),
            Some(Kept::TurnedOff { first: true })
        );
        assert!(!ON.with(Cell::get));
        assert_eq!(
            night_shift.keep(true, true),
            None,
            "not looked at again within seconds"
        );
        assert_eq!(look(&mut night_shift, true, true), None, "off already");

        ON.with(|o| o.set(true));
        assert_eq!(
            look(&mut night_shift, true, true),
            Some(Kept::TurnedOff { first: false }),
            "on again mid-show: off again, not said again"
        );

        assert_eq!(
            look(&mut night_shift, false, true),
            Some(Kept::GivenBack { on: true })
        );
        assert!(ON.with(Cell::get), "back on: night");
        let asked = ASKED.with(Cell::get);
        assert_eq!(look(&mut night_shift, false, true), None);
        assert_eq!(ASKED.with(Cell::get), asked, "nothing held, nothing asked");
    }

    /// Given back by day it stays off: its schedule turns it on at sunset.
    /// Off at the switch's word, it is given back too; off already, Nida
    /// holds nothing.
    #[test]
    fn given_back_by_day_it_stays_off() {
        let mut night_shift = fake();
        assert!(matches!(
            look(&mut night_shift, true, true),
            Some(Kept::TurnedOff { .. })
        ));
        NIGHT.with(|n| n.set(Some(false)));
        assert_eq!(
            look(&mut night_shift, true, false),
            Some(Kept::GivenBack { on: false }),
            "the switch turned off"
        );
        assert!(!ON.with(Cell::get), "day: left off");

        assert_eq!(look(&mut night_shift, true, true), None, "off already");
        assert_eq!(night_shift.give_back_now(), None, "nothing held");
    }

    /// As Nida quits, what it holds is given back.
    #[test]
    fn quitting_gives_it_back() {
        let mut night_shift = fake();
        SCHEDULE.with(|s| {
            s.set(Schedule::Hours {
                from: (20, 0),
                to: (6, 0),
            });
        });
        assert!(matches!(
            look(&mut night_shift, true, true),
            Some(Kept::TurnedOff { first: true })
        ));
        let set = SET.with(Cell::get);
        assert_eq!(
            night_shift.give_back_now(),
            Some(Kept::GivenBack { on: true })
        );
        assert_eq!(SET.with(Cell::get), set + 1, "turned on: within its hours");
        assert_eq!(night_shift.give_back_now(), None, "once");
    }

    /// Turning it back on refused: still held, tried again, given back once
    /// it takes.
    #[test]
    fn a_refused_give_back_is_tried_again() {
        let mut night_shift = NightShift {
            set_on: |on| {
                if on && REFUSE.with(Cell::get) {
                    return false;
                }
                fake_set(on)
            },
            ..fake()
        };
        assert!(matches!(
            look(&mut night_shift, true, true),
            Some(Kept::TurnedOff { .. })
        ));
        REFUSE.with(|r| r.set(true));
        assert_eq!(look(&mut night_shift, false, true), None, "refused");
        assert!(!ON.with(Cell::get));
        REFUSE.with(|r| r.set(false));
        assert_eq!(
            look(&mut night_shift, false, true),
            Some(Kept::GivenBack { on: true }),
            "tried again"
        );
        assert!(ON.with(Cell::get));
    }

    /// The runner's keeper, in a unit test, reads nothing of this Mac.
    #[test]
    fn the_default_keeper_reads_nothing_in_tests() {
        let mut night_shift = NightShift::default();
        assert_eq!(night_shift.keep(true, true), None);
        assert_eq!(night_shift.give_back_now(), None);
    }

    /// This Mac's own Night Shift and sun, read — never set — through the
    /// real calls: `cargo test --lib reads_this_macs -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "reads this Mac's Night Shift"]
    fn reads_this_macs_night_shift() {
        let status = super::platform::status();
        let night = super::platform::is_night();
        println!("Night Shift: {status:?}; night by the sun: {night:?}");
        if cfg!(target_os = "macos") {
            assert!(status.is_some(), "CoreBrightness answers");
            assert!(night.is_some(), "the sun's times are known");
        }
    }
}
