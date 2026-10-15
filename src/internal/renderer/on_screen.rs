//! How evenly a window's frames reached the screen, from Metal's own report
//! of when each was shown (`MTLDrawable.presentedTime`, read through the
//! vendored wgpu-hal's `Surface::take_presented_times`).
//!
//! The frame loop's own timings say when a frame was handed over; only this
//! says what the screen did with it — whether a slide moved evenly, which is
//! what an eye on the TV sees. A frame that stays up half a frame too long is
//! a hitch (the one before it repeated); one replaced half a frame early was
//! shown for almost nothing.

use std::collections::VecDeque;

use crate::engine::OnScreenStats;

/// On-screen intervals kept for the mean and spread.
const KEPT: usize = 300;
/// A gap this long is a pause — the window hidden, the screen asleep — not
/// a hitch.
const PAUSE: f64 = 1.0;

/// One window's on-screen record since it opened.
#[derive(Debug, Default)]
pub struct OnScreen {
    last: Option<f64>,
    recent: VecDeque<f64>,
    shown: u64,
    dropped: u64,
    hitches: u64,
    early: u64,
    expected: f64,
}

impl OnScreen {
    /// Take when frames reached the screen — host seconds, oldest first, 0
    /// for one never shown — for a window whose frames should each stay up
    /// `expected` seconds.
    pub fn record(&mut self, times: &[f64], expected: f64) {
        self.expected = expected;
        for &shown in times {
            if shown <= 0.0 {
                self.dropped += 1;
                continue;
            }
            if let Some(last) = self.last {
                let gap = shown - last;
                if gap > 0.0 && gap < PAUSE {
                    if gap > expected * 1.5 {
                        self.hitches += 1;
                    } else if gap < expected * 0.5 {
                        self.early += 1;
                    }
                    if self.recent.len() == KEPT {
                        self.recent.pop_front();
                    }
                    self.recent.push_back(gap);
                }
                self.last = Some(last.max(shown));
            } else {
                self.last = Some(shown);
            }
            self.shown += 1;
        }
    }

    pub fn stats(&self) -> OnScreenStats {
        #[allow(clippy::cast_precision_loss)]
        let n = self.recent.len() as f64;
        let mean = if n > 0.0 {
            self.recent.iter().sum::<f64>() / n
        } else {
            0.0
        };
        let sd = if n > 0.0 {
            (self.recent.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / n).sqrt()
        } else {
            0.0
        };
        #[allow(clippy::cast_possible_truncation)]
        OnScreenStats {
            shown: self.shown,
            dropped: self.dropped,
            hitches: self.hitches,
            early: self.early,
            expected_ms: (self.expected * 1000.0) as f32,
            interval_mean_ms: (mean * 1000.0) as f32,
            interval_sd_ms: (sd * 1000.0) as f32,
        }
    }
}

/// A window's frames presented and not yet reported shown: its queue of
/// frames waiting to be shown.
#[derive(Debug, Default)]
pub struct Waiting {
    presented: u64,
    reported: u64,
}

impl Waiting {
    pub fn waiting(&self) -> u64 {
        self.presented.saturating_sub(self.reported)
    }

    pub fn presented(&mut self) {
        self.presented += 1;
    }

    /// `frames` more reported shown or dropped.
    pub fn reported(&mut self, frames: usize) {
        self.reported += frames as u64;
    }
}

/// Frames presented to a window, none of them shown, for this long — and at
/// least [`UNSHOWN_FRAMES`] of them — and the screen has stopped taking the
/// window's frames.
pub const UNSHOWN_TIME: std::time::Duration = std::time::Duration::from_secs(1);
/// See [`UNSHOWN_TIME`].
pub const UNSHOWN_FRAMES: u64 = 30;

/// Notices a window whose frames macOS takes and never shows. Frames are
/// presented only while the window is visible to the app (hidden, its
/// acquire is skipped), so presents with nothing shown mean the screen has
/// stopped taking them. Seen with Stage Manager on: after an app switch an
/// output window's every frame was dropped, the window in front on its
/// screen, until the window was made again.
#[derive(Debug)]
pub struct Unshown {
    /// When a frame was last shown, or the watch began.
    since: std::time::Instant,
    /// The window's presents by then.
    presents_then: u64,
}

impl Unshown {
    /// Watching from `now`, the window having had `presents` presents.
    pub fn new(now: std::time::Instant, presents: u64) -> Self {
        Self {
            since: now,
            presents_then: presents,
        }
    }

    /// Take the on-screen times just read (`shown`, 0 for a frame never
    /// shown) and the window's presents so far. True while the window's
    /// frames go unshown (see [`UNSHOWN_TIME`]).
    pub fn note(&mut self, now: std::time::Instant, presents: u64, shown: &[f64]) -> bool {
        if shown.iter().any(|&at| at > 0.0) {
            *self = Self::new(now, presents);
            return false;
        }
        presents.saturating_sub(self.presents_then) >= UNSHOWN_FRAMES
            && now.saturating_duration_since(self.since) >= UNSHOWN_TIME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presents a second or more with none shown: the screen has stopped
    /// taking the window's frames. A frame shown starts the count again; a
    /// window with nothing presented (hidden) is not counted at all.
    #[test]
    fn frames_presented_for_a_second_and_never_shown_are_noticed() {
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        let mut watch = Unshown::new(start, 0);
        assert!(!watch.note(at(500), 30, &[0.0; 30]), "half a second");
        assert!(watch.note(at(1000), 60, &[0.0; 30]), "a second, 60 frames");
        assert!(!watch.note(at(1100), 66, &[0.0, 42.0]), "one shown");
        assert!(
            !watch.note(at(2200), 66, &[]),
            "a second with nothing presented"
        );
        assert!(
            !watch.note(at(2300), 90, &[0.0; 24]),
            "24 unshown since the last shown"
        );
        assert!(
            watch.note(at(2400), 96, &[0.0; 6]),
            "30 unshown over a second"
        );
    }

    #[test]
    fn frames_wait_from_their_present_until_they_are_reported() {
        let mut queue = Waiting::default();
        queue.presented();
        queue.presented();
        queue.presented();
        assert_eq!(queue.waiting(), 3);
        queue.reported(2);
        assert_eq!(queue.waiting(), 1);
        queue.reported(5);
        assert_eq!(queue.waiting(), 0, "never below none");
    }

    const FRAME: f64 = 1.0 / 60.0;

    /// Times `gaps` frames apart, starting at 100 s.
    fn shown_at(gaps: &[f64]) -> Vec<f64> {
        let mut t = 100.0;
        let mut times = vec![t];
        for g in gaps {
            t += g * FRAME;
            times.push(t);
        }
        times
    }

    #[test]
    fn an_even_output_has_no_hitches() {
        let mut on = OnScreen::default();
        on.record(&shown_at(&[1.0; 59]), FRAME);
        let s = on.stats();
        assert_eq!((s.shown, s.hitches, s.early, s.dropped), (60, 0, 0, 0));
        assert!((s.interval_mean_ms - 16.667).abs() < 0.01, "{s:?}");
        assert!(s.interval_sd_ms < 0.001, "{s:?}");
    }

    /// A frame up for two (the one before it repeated) is a hitch; the one
    /// after, replaced at once, is early; one never shown is dropped.
    #[test]
    fn a_repeated_frame_is_a_hitch_and_a_frame_never_shown_is_dropped() {
        let mut on = OnScreen::default();
        let mut times = shown_at(&[1.0, 2.0, 0.25, 1.0]);
        times.insert(2, 0.0);
        on.record(&times, FRAME);
        let s = on.stats();
        assert_eq!((s.shown, s.hitches, s.early, s.dropped), (5, 1, 1, 1));
    }

    /// Reads come in batches: the gap across two reads counts too, and a
    /// long pause (the window hidden) is not a hitch.
    #[test]
    fn batches_join_up_and_a_pause_is_not_a_hitch() {
        let mut on = OnScreen::default();
        let times = shown_at(&[1.0, 1.0, 3.0, 1.0]);
        on.record(&times[..3], FRAME);
        on.record(&times[3..], FRAME);
        assert_eq!(on.stats().hitches, 1, "the 3-frame gap across the reads");
        let last = *times.last().unwrap();
        on.record(&[last + 5.0, last + 5.0 + FRAME], FRAME);
        assert_eq!(on.stats().hitches, 1, "a five-second pause is not counted");
    }
}
