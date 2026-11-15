//! Where each frame's time goes, part by part — the UI, the wait for the
//! GPU, the mixer, each window's wait for a frame to draw into — so a frame
//! that starts or ends late can be traced to the part that made it late.
//!
//! Every part's times are kept for the last frames (the performance figures
//! show each part's mean and worst), and a frame that ran long is kept whole,
//! part by part, and written to the log.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::engine::{FrameBefore, FramePart, PartStats, SlowFrame};

/// Slow frames kept for the performance figures.
const SLOW_KEPT: usize = 20;
/// Slow frames written to the log a minute, at most.
const LOGGED_A_MINUTE: u32 = 30;
/// Parts shorter than this are left out of a slow frame's breakdown.
const NOTABLE_MS: f32 = 0.3;

#[derive(Default)]
pub(crate) struct FrameParts {
    /// Whose frames these are, as the log says of a slow one: none for the
    /// show's own.
    of: Option<&'static str>,
    /// This frame's parts so far, in the order spent, in milliseconds.
    frame: Vec<(String, f32)>,
    started: Option<Instant>,
    /// From the refresh that called for this frame to its start.
    start_delay: Option<f32>,
    /// When the frame before started.
    last_start: Option<Instant>,
    /// When the frame before ended.
    last_end: Option<Instant>,
    /// The frame before, when it was not slow: how long it was busy, and
    /// its parts. A slow one is a slow frame of its own.
    before: Option<(f32, Vec<(String, f32)>)>,
    /// Each part's times over the last frames, and the frame it was last in.
    history: BTreeMap<String, (VecDeque<f32>, u64)>,
    frames: u64,
    slow: VecDeque<SlowFrame>,
    /// The minute the log's count is for, and the count.
    logged: Option<(Instant, u32)>,
    origin: Option<Instant>,
    /// Of this frame's time so far, what went waiting for a screen to give
    /// a frame to draw into (see [`FrameParts::add_wait`]), in milliseconds.
    waited: f32,
}

impl FrameParts {
    /// The parts of the frames of `what` — "control window": not the show's
    /// own, whose slow frames the log tells apart by it. A control window's
    /// frame late by one is no frame late on an output.
    pub fn of(what: &'static str) -> Self {
        Self {
            of: Some(what),
            ..Self::default()
        }
    }

    /// A frame starts, `start_delay` after the refresh that called for it
    /// (when a screen's refresh did).
    pub fn begin(&mut self, now: Instant, start_delay: Option<Duration>) {
        self.origin.get_or_insert(now);
        // A frame begun and given up (the control window's, when the show
        // is not handed to it) is the frame before this one, and has
        // nothing to say: the one that ended before it is not said for it.
        if self.started.is_some() {
            self.before = None;
        }
        self.frame.clear();
        self.started = Some(now);
        self.start_delay = start_delay.map(ms);
        self.waited = 0.0;
    }

    /// This frame's parts so far, in milliseconds.
    pub fn so_far(&self) -> Vec<(String, f32)> {
        self.frame.clone()
    }

    /// `took` spent on `part` this frame (added to what it had, if twice).
    pub fn add(&mut self, part: &str, took: Duration) {
        if self.started.is_none() {
            return;
        }
        let took = ms(took);
        match self.frame.iter_mut().find(|(name, _)| name == part) {
            Some((_, spent)) => *spent += took,
            None => self.frame.push((part.to_string(), took)),
        }
    }

    /// `took` spent on `part` waiting for a screen to give a frame to draw
    /// into: the screen's pace, not the frame's own work, so up to a frame's
    /// time of it is not what makes the frame long (see [`FrameParts::end`]).
    /// A window drawn every show frame waits so each time — a screen gives
    /// its frames at its own refresh — and each of its frames read as busy
    /// a whole frame's time: a control window's, 12,000 of them written to
    /// the log of a seven-hour show (an M5 Max, 2026-11-09). A wait longer
    /// than a frame's time still makes it long, part by part in the log.
    pub fn add_wait(&mut self, part: &str, took: Duration) {
        if self.started.is_none() {
            return;
        }
        self.waited += ms(took);
        self.add(part, took);
    }

    /// The frame is done. `frame_ms` is a frame's time at the target rate:
    /// a frame busy for most of it — its waits for a screen aside, up to a
    /// frame's time of them — started half of it late, or started half as
    /// late again as it should have is slow.
    ///
    /// A slow frame that came or started late takes the frame before with
    /// it, when that one was not slow itself and started less than a second
    /// before it. A frame that waits 26 ms for its screen is not slow — up
    /// to a frame's time of the wait is the screen's pace — and the one
    /// after it comes late: its line said only that it did, and nothing of
    /// what the frame before had waited for or worked on (12 of the 21 late
    /// moments of a control window in an hour's show, an M5 Max,
    /// 2026-11-10).
    pub fn end(&mut self, now: Instant, frame_ms: f32) {
        let Some(started) = self.started.take() else {
            return;
        };
        let before = self.before.take();
        self.frames += 1;
        let busy = ms(now.saturating_duration_since(started));
        let interval = self
            .last_start
            .replace(started)
            .map(|last| ms(started.saturating_duration_since(last)));
        let between = self
            .last_end
            .replace(now)
            .map(|end| ms(started.saturating_duration_since(end)));
        let mut parts = std::mem::take(&mut self.frame);
        if let Some(delay) = self.start_delay {
            parts.insert(0, ("start delay".to_string(), delay));
        }
        for (name, spent) in &parts {
            let (times, seen) = self.history.entry(name.clone()).or_default();
            if times.len() >= super::PACING_FRAMES {
                times.pop_front();
            }
            times.push_back(*spent);
            *seen = self.frames;
        }
        // Parts gone for a while (a window closed) are forgotten.
        let frames = self.frames;
        self.history
            .retain(|_, (_, seen)| frames - *seen <= super::PACING_FRAMES as u64);

        let late_start = self.start_delay.is_some_and(|d| d > frame_ms * 0.5);
        let long = busy - self.waited.min(frame_ms) > frame_ms * 0.9;
        // The first after a pause (a launch, a dialog) is not a frame.
        let late = interval.is_some_and(|i| i > frame_ms * 1.5 && i < 1000.0);
        if !(late_start || long || late) {
            self.before = Some((busy, parts));
            return;
        }
        // A frame that only ran long says why itself; and what was before
        // a pause is not the frame before.
        let before = before
            .filter(|_| (late || late_start) && interval.is_some_and(|i| i < 1000.0))
            .map(|(busy_ms, parts)| FrameBefore {
                busy_ms,
                parts: notable(parts),
            });
        let slow = SlowFrame {
            at_secs: self
                .origin
                .map_or(0.0, |o| started.saturating_duration_since(o).as_secs_f64()),
            interval_ms: interval.unwrap_or(0.0),
            between_ms: between.unwrap_or(0.0),
            start_delay_ms: self.start_delay,
            busy_ms: busy,
            parts: notable(parts),
            before,
        };
        self.log(&slow, now);
        if self.slow.len() >= SLOW_KEPT {
            self.slow.pop_front();
        }
        self.slow.push_back(slow);
    }

    fn log(&mut self, slow: &SlowFrame, now: Instant) {
        let (minute, count) = self.logged.get_or_insert((now, 0));
        if now.saturating_duration_since(*minute) >= Duration::from_secs(60) {
            *minute = now;
            *count = 0;
        }
        *count += 1;
        if *count > LOGGED_A_MINUTE {
            return;
        }
        log::info!("{}", self.line(slow));
    }

    /// The log's line for `slow`: whose frame it was, when not the show's
    /// own, then where its time went.
    fn line(&self, slow: &SlowFrame) -> String {
        match self.of {
            Some(what) => format!("[PACING] slow frame ({what}): {}", describe(slow)),
            None => format!("[PACING] slow frame: {}", describe(slow)),
        }
    }

    /// Each part's mean and worst over the last frames, the costliest first.
    pub fn stats(&self) -> Vec<PartStats> {
        let mut stats: Vec<PartStats> = self
            .history
            .iter()
            .filter(|(_, (times, _))| !times.is_empty())
            .map(|(name, (times, _))| {
                #[allow(clippy::cast_precision_loss)]
                let mean = times.iter().sum::<f32>() / times.len() as f32;
                PartStats {
                    name: name.clone(),
                    mean_ms: mean,
                    max_ms: times.iter().copied().fold(0.0, f32::max),
                }
            })
            .collect();
        stats.sort_by(|a, b| b.mean_ms.total_cmp(&a.mean_ms));
        stats
    }

    /// The last frames that ran long, the latest last.
    pub fn slow_frames(&self) -> Vec<SlowFrame> {
        self.slow.iter().cloned().collect()
    }
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

/// Of a frame's parts, those a slow frame's breakdown names: the start
/// delay is said apart.
fn notable(parts: Vec<(String, f32)>) -> Vec<FramePart> {
    parts
        .into_iter()
        .filter(|(name, spent)| *spent >= NOTABLE_MS && name != "start delay")
        .map(|(name, ms)| FramePart { name, ms })
        .collect()
}

/// "busy 29.0 ms: gpu wait 14.2, ui 6.1" — the parts costliest first; the
/// time alone when no part is worth naming.
fn busy(busy_ms: f32, parts: &[FramePart]) -> String {
    let mut parts: Vec<&FramePart> = parts.iter().collect();
    parts.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let parts: Vec<String> = parts
        .iter()
        .map(|p| format!("{} {:.1}", p.name, p.ms))
        .collect();
    if parts.is_empty() {
        format!("busy {busy_ms:.1} ms")
    } else {
        format!("busy {busy_ms:.1} ms: {}", parts.join(", "))
    }
}

/// "31.2 ms after the last, 2.1 of it between frames (started 0.4 ms after
/// its refresh), busy 29.0 ms: gpu wait 14.2, ui 6.1, …" — the parts
/// costliest first. The time between frames is no part's: a frame that came
/// 32 ms after the last, busy 2 ms, said nothing of where the rest went (an
/// M5 Max, 2026-11-10) — into the frame before, or between the two. And
/// the frame before, when the slow one has it, follows: "; the frame
/// before: busy 29.0 ms: ui acquire 26.0, ui 3.0".
pub(crate) fn describe(slow: &SlowFrame) -> String {
    let start = slow
        .start_delay_ms
        .map(|d| format!(" (started {d:.1} ms after its refresh)"))
        .unwrap_or_default();
    let before = slow
        .before
        .as_ref()
        .map(|b| format!("; the frame before: {}", busy(b.busy_ms, &b.parts)))
        .unwrap_or_default();
    format!(
        "{:.1} ms after the last, {:.1} of it between frames{start}, {}{before}",
        slow.interval_ms,
        slow.between_ms,
        busy(slow.busy_ms, &slow.parts)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(parts: &mut FrameParts, at: Instant, spent: &[(&str, u64)]) -> Instant {
        parts.begin(at, Some(Duration::from_micros(200)));
        let mut now = at;
        for (name, micros) in spent {
            let took = Duration::from_micros(*micros);
            parts.add(name, took);
            now += took;
        }
        parts.end(now, 16.7);
        now
    }

    #[test]
    fn a_long_frame_is_kept_part_by_part() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        frame(&mut parts, t0, &[("ui", 4000), ("gpu wait", 2000)]);
        frame(&mut parts, t0 + step, &[("ui", 4000), ("gpu wait", 2000)]);
        assert!(parts.slow_frames().is_empty(), "on time");
        frame(&mut parts, t0 + step * 2, &[("ui", 4000), ("gpu wait", 20_000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1);
        assert!((slow[0].busy_ms - 24.0).abs() < 0.01, "{:?}", slow[0]);
        assert!(describe(&slow[0]).contains("gpu wait 20.0, ui 4.0"));
        // The one after starts late, because this one ran long.
        frame(&mut parts, t0 + step * 2 + Duration::from_millis(26), &[("ui", 4000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 2);
        assert!(slow[1].interval_ms > 25.0);
        // Of its 26 ms after the last, 24 were the last's own: 2 between.
        assert!((slow[1].between_ms - 2.0).abs() < 0.01, "{:?}", slow[1]);
    }

    /// A frame that starts late after a short one before it — the time went
    /// between the two, on no part of either — says so.
    #[test]
    fn a_frame_late_with_nothing_long_says_the_time_went_between_frames() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        frame(&mut parts, t0, &[("ui", 2000)]);
        frame(&mut parts, t0 + step, &[("ui", 2000)]);
        assert!(parts.slow_frames().is_empty(), "on time");
        frame(&mut parts, t0 + step + Duration::from_millis(32), &[("ui", 2000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1);
        assert!((slow[0].interval_ms - 32.0).abs() < 0.01, "{:?}", slow[0]);
        assert!((slow[0].between_ms - 30.0).abs() < 0.01, "{:?}", slow[0]);
        assert!(
            describe(&slow[0]).starts_with("32.0 ms after the last, 30.0 of it between frames"),
            "{}",
            describe(&slow[0])
        );
    }

    /// The log says whose frame was slow: the control window's is no frame
    /// late on an output, and a line that did not say which it was could
    /// not be read against the show's own parts.
    #[test]
    fn a_slow_frame_of_the_control_window_is_logged_as_its_own() {
        let late = |parts: &mut FrameParts| {
            let t0 = Instant::now();
            frame(parts, t0, &[("ui", 2000)]);
            frame(parts, t0 + Duration::from_millis(32), &[("ui", 2000)]);
            let slow = parts.slow_frames();
            assert_eq!(slow.len(), 1);
            parts.line(&slow[0])
        };
        let line = late(&mut FrameParts::of("control window"));
        assert!(
            line.starts_with(
                "[PACING] slow frame (control window): 32.0 ms after the last, 30.0 of it between frames"
            ),
            "{line}"
        );
        let line = late(&mut FrameParts::default());
        assert!(
            line.starts_with("[PACING] slow frame: 32.0 ms after the last, 30.0 of it between frames"),
            "{line}"
        );
    }

    /// A control window's frame: `work` on the UI, then `wait` for the
    /// window's next frame to draw into, in microseconds.
    fn waiting_frame(parts: &mut FrameParts, at: Instant, work: u64, wait: u64) {
        parts.begin(at, None);
        parts.add("ui", Duration::from_micros(work));
        parts.add_wait("ui acquire", Duration::from_micros(wait));
        parts.end(at + Duration::from_micros(work + wait), 16.7);
    }

    /// A window drawn every show frame waits for its screen each time: 4 ms
    /// of work and 12 of waiting is a frame's time gone, and not a slow
    /// frame. One whose work is long is, and one that waits far longer than
    /// a frame — with the wait among its parts.
    #[test]
    fn a_wait_for_the_screen_does_not_make_a_frame_long() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        for n in 0..4 {
            waiting_frame(&mut parts, t0 + step * n, 4000, 12_000);
        }
        assert!(parts.slow_frames().is_empty(), "{:?}", parts.slow_frames());
        let waited = parts.stats();
        let acquire = waited.iter().find(|s| s.name == "ui acquire").unwrap();
        assert!((acquire.mean_ms - 12.0).abs() < 0.01, "still among its parts");

        waiting_frame(&mut parts, t0 + step * 4, 15_500, 500);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "long work is slow: {slow:?}");
        assert!((slow[0].busy_ms - 16.0).abs() < 0.01);

        waiting_frame(&mut parts, t0 + step * 5, 2000, 40_000);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 2, "a wait far past a frame's time is slow: {slow:?}");
        assert!(describe(&slow[1]).contains("ui acquire 40.0, ui 2.0"));

        // The wait is this frame's alone: the next, all work, is long.
        parts.begin(t0 + step * 9, None);
        parts.add("ui", Duration::from_millis(16));
        parts.end(t0 + step * 9 + Duration::from_millis(16), 16.7);
        assert_eq!(parts.slow_frames().len(), 3);
    }

    /// A frame that waits 26 ms for its screen is not slow, and the one
    /// after it comes late: its line says what the frame before did — the
    /// wait, the work, nothing too short to name, and not its start delay.
    #[test]
    fn a_late_frame_says_what_the_frame_before_did() {
        let mut parts = FrameParts::of("control window");
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        waiting_frame(&mut parts, t0, 4000, 12_000);
        waiting_frame(&mut parts, t0 + step, 4000, 12_000);
        // 3 ms of work, 26 ms waiting, started a millisecond after its refresh.
        let at = t0 + step * 2;
        parts.begin(at, Some(Duration::from_millis(1)));
        parts.add("ui", Duration::from_millis(3));
        parts.add("meters", Duration::from_micros(100));
        parts.add_wait("ui acquire", Duration::from_micros(25_900));
        parts.end(at + Duration::from_millis(29), 16.7);
        assert!(parts.slow_frames().is_empty(), "{:?}", parts.slow_frames());

        waiting_frame(&mut parts, at + Duration::from_micros(29_500), 2000, 1000);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!((slow[0].interval_ms - 29.5).abs() < 0.01, "{:?}", slow[0]);
        let before = slow[0].before.as_ref().expect("the frame before");
        assert!((before.busy_ms - 29.0).abs() < 0.01, "{before:?}");
        let names: Vec<&str> = before.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ui", "ui acquire"], "in the order spent");
        let line = parts.line(&slow[0]);
        assert!(
            line.ends_with("busy 3.0 ms: ui 2.0, ui acquire 1.0; the frame before: busy 29.0 ms: ui acquire 25.9, ui 3.0"),
            "{line}"
        );
    }

    /// The frame before is said once: a slow one has its own line, and the
    /// late frame after it says nothing of it — nor of the one before that.
    /// A frame that ran long, on time, says why itself.
    #[test]
    fn a_slow_frame_before_is_in_its_own_line_only() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        frame(&mut parts, t0, &[("ui", 4000)]);
        frame(&mut parts, t0 + step, &[("ui", 4000), ("gpu wait", 20_000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!(slow[0].before.is_none(), "long, and on time: {:?}", slow[0]);
        frame(&mut parts, t0 + step + Duration::from_millis(26), &[("ui", 4000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 2, "{slow:?}");
        assert!(slow[1].before.is_none(), "{:?}", slow[1]);
        assert!(!describe(&slow[1]).contains("the frame before"));
    }

    /// What was before a pause is not the frame before: a frame that starts
    /// late after its refresh says the frame before when that was a frame's
    /// time ago, not when it was five seconds ago.
    #[test]
    fn a_frame_before_a_pause_is_not_the_frame_before() {
        let late_start = |parts: &mut FrameParts, at: Instant| {
            parts.begin(at, Some(Duration::from_millis(10)));
            parts.add("ui", Duration::from_millis(2));
            parts.end(at + Duration::from_millis(2), 16.7);
        };
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        let step = Duration::from_micros(16_667);
        frame(&mut parts, t0, &[("ui", 2000)]);
        late_start(&mut parts, t0 + step);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!(slow[0].before.is_some(), "{:?}", slow[0]);
        assert!(
            describe(&slow[0]).ends_with("busy 2.0 ms: ui 2.0; the frame before: busy 2.0 ms: ui 2.0"),
            "{}",
            describe(&slow[0])
        );

        frame(&mut parts, t0 + step * 2, &[("ui", 2000)]);
        late_start(&mut parts, t0 + step * 2 + Duration::from_secs(5));
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 2, "{slow:?}");
        assert!(slow[1].before.is_none(), "{:?}", slow[1]);
    }

    /// A frame begun and given up is the frame before the next: the one
    /// that ended before it is not said in its place.
    #[test]
    fn a_frame_before_one_given_up_is_not_the_frame_before() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        frame(&mut parts, t0, &[("ui", 2000)]);
        parts.begin(t0 + Duration::from_millis(17), None);
        parts.add("ui", Duration::from_millis(1));
        frame(&mut parts, t0 + Duration::from_millis(50), &[("ui", 2000)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!((slow[0].interval_ms - 50.0).abs() < 0.01, "{:?}", slow[0]);
        assert!(slow[0].before.is_none(), "{:?}", slow[0]);
    }

    /// A frame with no part worth naming is its time alone.
    #[test]
    fn a_frame_of_no_notable_part_is_its_time_alone() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        frame(&mut parts, t0, &[("ui", 100)]);
        frame(&mut parts, t0 + Duration::from_millis(32), &[("ui", 100)]);
        let slow = parts.slow_frames();
        assert_eq!(slow.len(), 1, "{slow:?}");
        assert!(
            describe(&slow[0]).ends_with("busy 0.1 ms; the frame before: busy 0.1 ms"),
            "{}",
            describe(&slow[0])
        );
    }

    #[test]
    fn parts_have_a_mean_and_a_worst_the_costliest_first() {
        let mut parts = FrameParts::default();
        let t0 = Instant::now();
        for (i, gpu) in [1000_u64, 3000].iter().enumerate() {
            frame(
                &mut parts,
                t0 + Duration::from_millis(17 * i as u64),
                &[("ui", 5000), ("gpu wait", *gpu), ("ui", 1000)],
            );
        }
        let stats = parts.stats();
        assert_eq!(stats[0].name, "ui");
        assert!((stats[0].mean_ms - 6.0).abs() < 0.01, "twice in a frame adds up");
        let gpu = stats.iter().find(|s| s.name == "gpu wait").unwrap();
        assert!((gpu.mean_ms - 2.0).abs() < 0.01 && (gpu.max_ms - 3.0).abs() < 0.01);
        assert!(stats.iter().any(|s| s.name == "start delay"));
    }

    #[test]
    fn a_part_gone_for_a_while_is_forgotten() {
        let mut parts = FrameParts::default();
        let mut at = Instant::now();
        frame(&mut parts, at, &[("TV acquire", 100)]);
        for _ in 0..=super::super::PACING_FRAMES {
            at += Duration::from_millis(17);
            frame(&mut parts, at, &[("ui", 100)]);
        }
        assert!(parts.stats().iter().all(|s| s.name != "TV acquire"));
    }
}
