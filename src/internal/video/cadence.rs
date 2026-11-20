//! Fixed cadence accounting independent of the wait primitive.
#[cfg(any(windows, test))]
use std::time::{Duration, Instant};

/// PTS wakes must not bypass the existing output cap at faster playback speeds.
pub(super) fn picture_deadline_allowed(source_fps: f64, speed: f64, output_fps: u32) -> bool {
    output_fps == 0
        || (source_fps <= f64::from(output_fps)
            && source_fps * speed.abs() <= f64::from(output_fps))
}

/// Work consumes the remaining slot. Over-budget decoding waits zero rather
/// than paying another full interval and falling to half throughput.
#[cfg(any(windows, test))]
pub(super) fn next_wait(deadline: Instant, now: Instant) -> Duration {
    deadline.saturating_duration_since(now)
}

/// Consume due slots at wake-up. Early commands preserve the upcoming deadline;
/// a stall permits one catch-up iteration before restarting in the future.
#[cfg(any(windows, test))]
pub(super) fn on_wake(deadline: &mut Instant, interval: Duration, now: Instant) {
    if *deadline <= now {
        *deadline += interval;
        if *deadline <= now {
            *deadline = now + interval;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_deadlines_preserve_source_and_effective_rate_caps() {
        assert!(picture_deadline_allowed(60.0, 1.0, 60));
        assert!(picture_deadline_allowed(30.0, 2.0, 60));
        assert!(picture_deadline_allowed(60.0, 0.5, 60));
        assert!(!picture_deadline_allowed(60.0, 2.0, 60));
        assert!(!picture_deadline_allowed(30.0, 4.0, 60));
        assert!(!picture_deadline_allowed(75.0, 0.5, 60));
        assert!(picture_deadline_allowed(75.0, 2.0, 0));
    }

    #[test]
    fn over_budget_decode_does_not_add_another_frame_of_wait() {
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let decode = Duration::from_millis(17);
        let mut now = start;
        let mut deadline = start + interval;
        for _ in 0..100 {
            now += decode;
            let wait = next_wait(deadline, now);
            assert_eq!(
                wait,
                Duration::ZERO,
                "a decoder already over budget must not be paced at half throughput"
            );
            now += wait;
            on_wake(&mut deadline, interval, now);
        }
    }

    #[test]
    fn early_commands_preserve_the_pending_deadline() {
        let start = Instant::now();
        let interval = Duration::from_millis(10);
        let mut deadline = start + interval;
        for millis in 1..10 {
            let now = start + Duration::from_millis(millis);
            assert_eq!(next_wait(deadline, now), deadline - now);
            on_wake(&mut deadline, interval, now);
            assert_eq!(deadline, start + interval);
        }
        on_wake(&mut deadline, interval, start + interval);
        assert_eq!(next_wait(deadline, start + interval), interval);
        assert_eq!(deadline, start + interval * 2);
    }

    #[test]
    fn decoding_uses_the_remaining_slot_and_stalls_do_not_burst() {
        let start = Instant::now();
        let interval = Duration::from_millis(10);
        let mut deadline = start + interval;
        for millis in [4, 14, 24] {
            let now = start + Duration::from_millis(millis);
            let wait = next_wait(deadline, now);
            assert_eq!(wait, Duration::from_millis(6));
            on_wake(&mut deadline, interval, now + wait);
        }
        let stalled = start + Duration::from_secs(1);
        assert_eq!(next_wait(deadline, stalled), Duration::ZERO);
        on_wake(&mut deadline, interval, stalled);
        assert_eq!(deadline, stalled + interval);
    }
}
