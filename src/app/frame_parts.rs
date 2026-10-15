//! Where each frame's time goes, part by part — the UI, the wait for the
//! GPU, the mixer, each window's wait for a frame to draw into — so a frame
//! that starts or ends late can be traced to the part that made it late.
//!
//! Every part's times are kept for the last frames (the performance figures
//! show each part's mean and worst), and a frame that ran long is kept whole,
//! part by part, and written to the log.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use crate::engine::{FramePart, PartStats, SlowFrame};

/// Slow frames kept for the performance figures.
const SLOW_KEPT: usize = 20;
/// Slow frames written to the log a minute, at most.
const LOGGED_A_MINUTE: u32 = 30;
/// Parts shorter than this are left out of a slow frame's breakdown.
const NOTABLE_MS: f32 = 0.3;

#[derive(Default)]
pub(crate) struct FrameParts {
    /// This frame's parts so far, in the order spent, in milliseconds.
    frame: Vec<(String, f32)>,
    started: Option<Instant>,
    /// From the refresh that called for this frame to its start.
    start_delay: Option<f32>,
    /// When the frame before started.
    last_start: Option<Instant>,
    /// Each part's times over the last frames, and the frame it was last in.
    history: BTreeMap<String, (VecDeque<f32>, u64)>,
    frames: u64,
    slow: VecDeque<SlowFrame>,
    /// The minute the log's count is for, and the count.
    logged: Option<(Instant, u32)>,
    origin: Option<Instant>,
}

impl FrameParts {
    /// A frame starts, `start_delay` after the refresh that called for it
    /// (when a screen's refresh did).
    pub fn begin(&mut self, now: Instant, start_delay: Option<Duration>) {
        self.origin.get_or_insert(now);
        self.frame.clear();
        self.started = Some(now);
        self.start_delay = start_delay.map(ms);
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

    /// The frame is done. `frame_ms` is a frame's time at the target rate:
    /// a frame busy for most of it, started half of it late, or started half
    /// as late again as it should have is slow.
    pub fn end(&mut self, now: Instant, frame_ms: f32) {
        let Some(started) = self.started.take() else {
            return;
        };
        self.frames += 1;
        let busy = ms(now.saturating_duration_since(started));
        let interval = self
            .last_start
            .replace(started)
            .map(|last| ms(started.saturating_duration_since(last)));
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
        let long = busy > frame_ms * 0.9;
        // The first after a pause (a launch, a dialog) is not a frame.
        let late = interval.is_some_and(|i| i > frame_ms * 1.5 && i < 1000.0);
        if !(late_start || long || late) {
            return;
        }
        let slow = SlowFrame {
            at_secs: self
                .origin
                .map_or(0.0, |o| started.saturating_duration_since(o).as_secs_f64()),
            interval_ms: interval.unwrap_or(0.0),
            start_delay_ms: self.start_delay,
            busy_ms: busy,
            parts: parts
                .into_iter()
                .filter(|(name, spent)| *spent >= NOTABLE_MS && name != "start delay")
                .map(|(name, ms)| FramePart { name, ms })
                .collect(),
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
        log::info!("[PACING] slow frame: {}", describe(slow));
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

/// "31.2 ms after the last (started 0.4 ms after its refresh), busy 29.0 ms:
/// gpu wait 14.2, ui 6.1, …" — the parts costliest first.
pub(crate) fn describe(slow: &SlowFrame) -> String {
    let mut parts: Vec<&FramePart> = slow.parts.iter().collect();
    parts.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    let parts: Vec<String> = parts
        .iter()
        .map(|p| format!("{} {:.1}", p.name, p.ms))
        .collect();
    let start = slow
        .start_delay_ms
        .map(|d| format!(" (started {d:.1} ms after its refresh)"))
        .unwrap_or_default();
    format!(
        "{:.1} ms after the last{start}, busy {:.1} ms: {}",
        slow.interval_ms,
        slow.busy_ms,
        parts.join(", ")
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
