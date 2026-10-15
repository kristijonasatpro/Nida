//! The host clock, in seconds: the one macOS stamps frames with — Metal's
//! presented and GPU times, a captured screen's display times, a camera's
//! sample times — so times from any of them, and from the show, compare.

use std::time::Instant;

/// The host clock now, seconds: on macOS `mach_absolute_time`'s; elsewhere
/// since Nida started.
pub fn host_seconds() -> f64 {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: no arguments; reads the host clock.
        #[allow(deprecated)]
        let ticks = unsafe { libc::mach_absolute_time() };
        seconds_of_ticks(ticks)
    }
    #[cfg(not(target_os = "macos"))]
    {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_secs_f64()
    }
}

/// `ticks` of `mach_absolute_time`, as seconds on the host clock.
// libc marks its mach calls deprecated in favour of the mach2 crate, which
// Nida does not use (as in the display link's `vsync.rs`).
#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn seconds_of_ticks(ticks: u64) -> f64 {
    static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 1, denom: 1 };
        // SAFETY: `info` is a valid place for the answer.
        unsafe { libc::mach_timebase_info(&raw mut info) };
        (info.numer, info.denom.max(1))
    });
    #[allow(clippy::cast_precision_loss)]
    let nanos = ticks as f64 * f64::from(numer) / f64::from(denom);
    nanos / 1e9
}

/// `at` on the host clock (see [`host_seconds`]).
pub fn host_seconds_of(at: Instant) -> f64 {
    let now = Instant::now();
    let host = host_seconds();
    if at <= now {
        host - (now - at).as_secs_f64()
    } else {
        host + (at - now).as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An instant and the host clock agree to well within a millisecond,
    /// before and after now.
    #[test]
    fn an_instant_is_put_on_the_host_clock() {
        let now = Instant::now();
        let host = host_seconds();
        let half = std::time::Duration::from_millis(500);
        let before = host_seconds_of(now.checked_sub(half).expect("up half a second"));
        let after = host_seconds_of(now + half);
        assert!((host - before - 0.5).abs() < 0.001, "{}", host - before);
        assert!((after - host - 0.5).abs() < 0.001, "{}", after - host);
    }
}
