//! One optional publication-wait window shared by an entire video update batch.
use std::time::{Duration, Instant};

// The 1 ms candidate recovered the phase race but one measured copy arrived
// 1.281 ms after the attempt. Keep a small margin within one shared window.
const WINDOW: Duration = Duration::from_micros(1500);

pub(crate) struct FrameReadiness {
    output_fps: u32,
    pub(super) transport_running: bool,
    deadline: Option<Instant>,
}

impl FrameReadiness {
    pub(crate) fn new(output_fps: u32, transport_running: bool) -> Self {
        #[cfg(test)]
        let output_fps = if std::env::var("NIDA_VIDEO_BENCH_NO_READINESS").is_ok_and(|v| v == "1") {
            0
        } else {
            output_fps
        };
        Self {
            output_fps,
            transport_running,
            deadline: None,
        }
    }

    pub(crate) fn disabled() -> Self {
        Self::new(0, false)
    }

    pub(super) fn enabled(&self) -> bool {
        self.output_fps != 0
    }

    pub(super) fn deadline(&mut self, source_fps: f64, speed: f64) -> Option<Instant> {
        self.deadline_at(source_fps, speed, Instant::now())
    }

    fn deadline_at(&mut self, source_fps: f64, speed: f64, now: Instant) -> Option<Instant> {
        // Rate conversion legitimately holds pictures. Do not wait on every
        // empty output tick of, for example, a 24 fps source on a 60 Hz output.
        let rate = source_fps * speed;
        let output = f64::from(self.output_fps);
        // Above the output cap the worker uses periodic updates. A 60 fps
        // source at 2x still produces 60 payloads/s; a 30 fps source at 4x
        // retains its 30 Hz periodic policy and legitimately has empty ticks.
        let worker_reaches_output = source_fps >= output
            || super::cadence::picture_deadline_allowed(source_fps, speed, self.output_fps);
        if self.output_fps == 0
            || !rate.is_finite()
            || speed <= 0.0
            || rate < output - 0.001
            || !worker_reaches_output
        {
            return None;
        }
        let deadline = *self.deadline.get_or_insert(now + WINDOW);
        (now < deadline).then_some(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_decks_share_one_window_that_never_restarts() {
        let start = Instant::now();
        let mut budget = FrameReadiness::new(60, false);
        let deadline = budget.deadline_at(60.0, 1.0, start).unwrap();
        for micros in [100, 400, 900, 1400] {
            assert_eq!(
                budget.deadline_at(30.0, 2.0, start + Duration::from_micros(micros)),
                Some(deadline)
            );
        }
        assert_eq!(budget.deadline_at(60.0, 1.0, deadline), None);
        assert_eq!(
            budget.deadline_at(60.0, 1.0, deadline + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn rate_conversion_and_disabled_batches_do_not_start_a_window() {
        let start = Instant::now();
        let mut budget = FrameReadiness::new(60, false);
        for (fps, speed) in [
            (24.0, 1.0),
            (30.0, 4.0),
            (60.0, 0.5),
            (60.0, -1.0),
            (f64::NAN, 1.0),
        ] {
            assert_eq!(budget.deadline_at(fps, speed, start), None);
        }
        assert_eq!(budget.deadline, None);
        assert_eq!(
            FrameReadiness::disabled().deadline_at(60.0, 1.0, start),
            None
        );
        assert_eq!(budget.deadline_at(60.0, 1.0, start), Some(start + WINDOW));
    }

    #[test]
    fn output_capped_fast_playback_can_wait_without_changing_decode_caps() {
        for (fps, speed) in [(60.0, 2.0), (120.0, 0.5), (30.0, 2.0), (75.0, 1.0)] {
            let start = Instant::now();
            assert_eq!(
                FrameReadiness::new(60, false).deadline_at(fps, speed, start),
                Some(start + WINDOW)
            );
        }
    }
}
