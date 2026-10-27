//! Which of a live input's frames each show frame shows: picked by when each
//! was captured, not as it happened to arrive.
//!
//! A camera or a captured screen sends frames on a clock of its own — the
//! camera's, the captured screen's refresh — near the show's rate but never
//! exactly, and each frame arrives a little early or late. The show took the
//! newest to have arrived as each of its frames was drawn: an input whose
//! frames arrived near that moment had the two change order from frame to
//! frame, and its picture showed some frames twice and others never — a
//! judder whenever something moved in front of a camera (an ATEM, a laptop,
//! 2026-10-02). Picked by capture time instead — for each show frame, the
//! newest input frame captured a steady delay before it — the picture moves
//! evenly; only a real difference of rate shows, as one frame repeated or
//! skipped now and then, never two together.
//!
//! The delay is how old each frame was when it was first there to show, at
//! worst lately — leaving out the few latest of all, so one frame held up
//! does not hold back the ones after it: frames that always come in time are
//! shown as soon as they come, as the newest to have arrived was — no later —
//! and only an input whose frames often come too late is shown a frame later,
//! the one that has always come. (Held half a frame more and kept to the
//! middle of each frame's turn, as this first was, it cost a camera a frame
//! or more on screen for nothing: "bigger delay then before … like 50 ms", a
//! laptop, 2026-10-03.) Only frames that come count: a captured screen
//! sends nothing while its picture stands still, and counting how old the
//! frame on screen grew meanwhile once held a slide's next frames back until
//! they were let go, the picture frozen for 14–22 s.

use std::collections::VecDeque;

/// Frames kept waiting at most: more lets the oldest go, counted as skipped
/// at the next pick.
const KEEP: usize = 16;

/// The longest delay kept behind the show, seconds: an input that keeps
/// coming later than this is shown as it comes.
const LONGEST_DELAY: f64 = 0.25;

/// How many of the latest frames' ages are kept to know how late the input
/// comes: two seconds of a 60 fps show.
const AGES_KEPT: usize = 120;

/// How long the input's frames must keep coming earlier than the delay
/// allows before it is shortened, seconds, at first: an input whose rate
/// drifts from the show's comes later and earlier in turn, and a delay that
/// followed it down and up repeated and skipped frames each time.
const LOWER_AFTER: f64 = 10.0;

/// A shortening the delay had to take back within this long, seconds, was
/// too soon: the wait before the next is doubled. Each time as long that a
/// shortening holds — no frame coming later than the delay allows — halves
/// the wait again, back to [`LOWER_AFTER`]: a camera that came late in one
/// bad patch, and in time since, is not kept waiting ten minutes for its
/// delay to come down after its next late frame.
const TOO_SOON: f64 = 60.0;

/// Late frames this close together, seconds, keep coming: a shortening
/// between two of them doubles the wait as one taken back too soon does, from
/// the wait as it was at the first — halving it meanwhile does not count. A
/// late frame every 80 s otherwise had the delay shortened 12 s after each
/// and raised again at the next, a frame skipped and one repeated each time
/// (codex, 2026-10-04).
const RECURRING: f64 = 120.0;

/// The longest wait before the delay is shortened, seconds.
const LONGEST_WAIT: f64 = 600.0;

/// How much later than the time wanted, in frames, the next frame in turn
/// may have been captured and still be shown; later, the frame before is
/// shown again.
const NEXT_UNTIL: f64 = 0.75;

/// How much later than the time wanted, in frames, a frame after the next
/// one in turn may have been captured and be shown in its place — those
/// between skipped.
///
/// The frame wanted is captured a hair after the time wanted, by as much as
/// the refresh call it is timed by came late — the delay is the oldest a
/// frame came at, the latest call among them. Taking the newest captured by
/// that time and 2 ms had the call's wobble, a millisecond or two, show one
/// frame twice and skip the next, up to seven times a second (a laptop,
/// 2026-10-04). A quarter of a frame either side of the halfway mark leaves
/// the wobble nothing to tip: the next frame in turn is shown until it is
/// three quarters of a frame early, and a later one only once that one is
/// due within a quarter of a frame.
const SKIP_UNTIL: f64 = 0.25;

/// A frame that came this many frames later than any other counted was held
/// up — a camera that stopped for a moment — and is left out of the delay:
/// the frames after it are shown as they come, the ones held up skipped
/// together. Frames that come late now and then by less, around the moment
/// show frames are drawn, are counted: leaving out one late frame in twenty
/// had those repeated and skipped in pairs.
const HELD_UP: f64 = 1.5;

/// Gaps too long to be the input's rate, in a row, after which they are its
/// rate: a camera gone from 60 to 30 fps.
const RELEARN_AFTER: usize = 30;

/// A frame captured this long before the newest come, seconds, is from a
/// clock that went back — a source started again — not one that came out of
/// turn.
const WENT_BACK: f64 = 0.5;

/// Picks, for each show frame, the frame of a live input to show (see the
/// module doc). Frames go in as they arrive, with when they were captured on
/// the host clock; one comes out per show frame, or none to keep showing the
/// one before.
#[derive(Debug)]
pub struct FramePicker<F> {
    /// Frames not yet shown, oldest captured first.
    waiting: VecDeque<(f64, F)>,
    /// When the frame shown last was captured.
    shown: Option<f64>,
    /// The time between the input's frames, as seen.
    period: Option<f64>,
    /// Gaps too long to be the input's rate seen in a row (see
    /// [`RELEARN_AFTER`]).
    long_gaps: usize,
    /// How long before each show frame's time its input frame was captured.
    delay: Option<f64>,
    /// The input's clock as followed: when its newest frame was captured,
    /// smoothed.
    clock: Option<f64>,
    /// How old each of the latest frames was when it was first there to
    /// show — at each pick, the oldest of those come since the one before.
    ages: VecDeque<f64>,
    /// When the newest frame counted in `ages` was captured.
    counted: Option<f64>,
    /// Since when (show time) the input's frames have come earlier than the
    /// delay allows, by half a frame or more.
    earlier_since: Option<f64>,
    /// How long they must, before the delay is shortened (see
    /// [`TOO_SOON`]).
    lower_after: f64,
    /// When the delay was last shortened.
    lowered_at: Option<f64>,
    /// Since when a shortening has held, if it has, for the wait to be
    /// halved each [`TOO_SOON`].
    held_since: Option<f64>,
    /// When the delay was last raised by a frame that came late, and the
    /// wait before shortening it then (see [`RECURRING`]).
    raised: Option<(f64, f64)>,
    /// How long before its show frame's time the frame shown last was
    /// captured.
    latency: Option<f64>,
    /// Frames let go unshown since the last pick: more were waiting than
    /// are kept.
    lost: usize,
    /// Frames kept waiting at most.
    keep: usize,
    /// Show the newest frame come at each pick, held to no delay (see
    /// [`FramePicker::set_newest`]).
    newest: bool,
}

impl<F> Default for FramePicker<F> {
    fn default() -> Self {
        Self {
            waiting: VecDeque::with_capacity(KEEP + 1),
            shown: None,
            period: None,
            long_gaps: 0,
            delay: None,
            clock: None,
            ages: VecDeque::with_capacity(AGES_KEPT + 1),
            counted: None,
            earlier_since: None,
            lower_after: LOWER_AFTER,
            lowered_at: None,
            held_since: None,
            raised: None,
            latency: None,
            lost: 0,
            keep: KEEP,
            newest: false,
        }
    }
}

/// What [`FramePicker::pick`] did, for a caller that counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    /// The next frame in turn.
    Next,
    /// A frame further on: the ones between were let go, the input being
    /// ahead of the show.
    Skipped(usize),
    /// None: the frame before is shown again, the input being behind.
    Repeated,
}

impl<F> FramePicker<F> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The frames waiting let go — a capture's, replaced by another — its
    /// timing kept.
    pub fn discard_waiting(&mut self) {
        self.waiting.clear();
    }

    /// One keeping at most `keep` frames waiting (at least 2): for frames
    /// that hold something their source needs back, as a captured screen's
    /// do its surfaces.
    pub fn keeping(keep: usize) -> Self {
        Self {
            keep: keep.max(2),
            ..Self::default()
        }
    }

    /// Show the newest frame come at each pick, rather than each frame in
    /// turn at the delay that keeps them even: about a frame sooner on
    /// screen for an input whose frames come unevenly — a camera over USB,
    /// some 5% of whose frames come more than 10 ms later than most — but
    /// each frame that comes late shown late, the one before repeated and
    /// the next skipped: two or so judders a second with that camera (a
    /// laptop, 2026-10-04). How late frames come is still followed, so
    /// turning it off is even at once.
    pub fn set_newest(&mut self, newest: bool) {
        self.newest = newest;
    }

    /// The newest frame waiting, if any.
    pub fn newest(&self) -> Option<&F> {
        self.waiting.back().map(|(_, frame)| frame)
    }

    /// How long before its show frame's time the frame shown last was
    /// captured, seconds: the input's delay on screen, past its own.
    pub fn latency(&self) -> Option<f64> {
        self.latency
    }

    /// When the frame shown last was captured, host seconds.
    pub fn shown(&self) -> Option<f64> {
        self.shown
    }

    /// How long before each show frame its frame is wanted captured, seconds.
    pub fn delay(&self) -> Option<f64> {
        self.delay
    }

    /// How many frames wait.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// A frame captured at `at`, host seconds. One captured within half a
    /// frame before the newest waiting replaces it (a repeat); one further
    /// back, out of turn, is let go, as is one no later than the frame shown;
    /// one from much further back — a clock that went back, a source started
    /// again (see [`WENT_BACK`]) — starts the picking afresh, as its frames
    /// would otherwise all be let go until the clock came back to where it
    /// was.
    ///
    /// The time is smoothed along the input's clock first: a camera stamps
    /// its frames a little unevenly.
    ///
    /// Returns the frames let go: to be dropped once the picker is no longer
    /// held — a frame can hold a picture's memory, or a captured screen's
    /// surface, which take a moment to give back, and the show's frame
    /// waits for the picker meanwhile.
    #[must_use = "frames let go are dropped once the picker is let go of"]
    pub fn push(&mut self, at: f64, frame: F) -> Vec<F> {
        let mut let_go = Vec::new();
        let at = self.follow_clock(at);
        let half = self.period.unwrap_or(1.0 / 60.0) * 0.5;
        if let Some(last) = self.waiting.back().map(|&(newest, _)| newest).or(self.shown) {
            if at < last - WENT_BACK {
                let_go.extend(self.start_afresh(at));
            } else if at <= last {
                if at < last - half {
                    // Out of turn: never shown, counted as skipped.
                    self.lost += 1;
                    let_go.push(frame);
                    return let_go;
                }
                // The newest again, its stamp wavered: it takes the newest's
                // place — only while it still comes after the frame before
                // that, waiting or shown, or it would be shown after a newer.
                let before = self
                    .waiting
                    .iter()
                    .rev()
                    .nth(1)
                    .map(|&(earlier, _)| earlier)
                    .or(self.shown);
                if self.waiting.is_empty() || before.is_some_and(|before| at <= before) {
                    let_go.push(frame);
                    return let_go;
                }
                let_go.extend(self.waiting.pop_back().map(|(_, replaced)| replaced));
            } else {
                self.see_period(at - last);
            }
        }
        self.waiting.push_back((at, frame));
        while self.waiting.len() > self.keep {
            let_go.extend(self.waiting.pop_front().map(|(_, old)| old));
            self.lost += 1;
        }
        let_go
    }

    /// `at` along the input's clock: where the frame after the last one
    /// falls, pulled a tenth of the way to `at` — or `at` itself when it is
    /// half a frame or more off, the input having dropped frames or started
    /// again.
    fn follow_clock(&mut self, at: f64) -> f64 {
        let followed = match (self.clock, self.period) {
            (Some(last), Some(period)) => {
                let frames = ((at - last) / period).round().max(1.0);
                let expected = last + frames * period;
                if (at - expected).abs() < period * 0.5 {
                    expected + (at - expected) * 0.1
                } else {
                    at
                }
            }
            _ => at,
        };
        self.clock = Some(followed);
        followed
    }

    /// Learn the time between frames from one seen: a gap of several frames
    /// (frames the input dropped) does not count — unless such gaps keep
    /// coming, and are the input's new rate.
    fn see_period(&mut self, gap: f64) {
        if !(1.0 / 480.0..=0.5).contains(&gap) {
            return;
        }
        self.period = Some(match self.period {
            None => gap,
            Some(p) if gap > p * 1.5 => {
                self.long_gaps += 1;
                if self.long_gaps < RELEARN_AFTER {
                    return;
                }
                gap
            }
            Some(p) => p + (gap - p) * 0.05,
        });
        self.long_gaps = 0;
    }

    /// Forget what the frames so far said — when they were shown, how late
    /// they came — for an input whose clock went back to `at`.
    fn start_afresh(&mut self, at: f64) -> Vec<F> {
        self.lost += self.waiting.len();
        let let_go = self.waiting.drain(..).map(|(_, frame)| frame).collect();
        self.shown = None;
        self.delay = None;
        self.ages.clear();
        self.counted = None;
        self.earlier_since = None;
        self.lower_after = LOWER_AFTER;
        self.lowered_at = None;
        self.held_since = None;
        self.raised = None;
        self.clock = Some(at);
        let_go
    }

    /// The frame for the show frame due on screen at `show_at`, host
    /// seconds, and what was done; `None` keeps the frame before.
    pub fn pick(&mut self, show_at: f64) -> (Option<F>, Picked) {
        // How old the frames come since the last pick are, now they are
        // here to show: the oldest of them, which came latest for its turn.
        // A frame that comes late is counted as late as it came; while none
        // comes, nothing is counted. Frames waiting before anything was shown
        // were there before the picking began: only the newest of those says
        // how late frames come.
        let counted = self.counted;
        let new = self
            .waiting
            .iter()
            .map(|&(at, _)| at)
            .find(|&at| counted.is_none_or(|c| at > c));
        if let (Some(oldest_new), Some(&(newest, _))) = (new, self.waiting.back()) {
            if self.ages.len() >= AGES_KEPT {
                self.ages.pop_front();
            }
            let came = if self.shown.is_some() { oldest_new } else { newest };
            self.ages.push_back(show_at - came);
            self.counted = Some(newest);
        }
        let Some(&(oldest, _)) = self.waiting.front() else {
            return (None, Picked::Repeated);
        };
        let delay = self.follow_delay(self.lately().min(LONGEST_DELAY), show_at);
        if self.newest {
            return self.take(self.waiting.len() - 1, show_at);
        }
        let wanted = show_at - delay;
        let frame = self.period.unwrap_or(1.0 / 60.0);

        // The next frame in turn, if not too early — or a later one, due as
        // much, those between let go (see [`SKIP_UNTIL`]).
        if oldest <= wanted + frame * NEXT_UNTIL {
            let chosen = self
                .waiting
                .iter()
                .rposition(|&(at, _)| at <= wanted + frame * SKIP_UNTIL)
                .unwrap_or(0);
            return self.take(chosen, show_at);
        }
        if self.shown.is_none() {
            // The very first frame: the oldest come, as none is old enough.
            return self.take(0, show_at);
        }
        if self.waiting.len() >= self.keep {
            // Wanting older than any frame kept would let them all go unshown:
            // the oldest is shown, the delay brought down to it, and how late
            // frames came forgotten where it asked for more than is kept.
            let most = show_at - oldest;
            self.delay = Some(most);
            self.earlier_since = None;
            self.ages.retain(|&age| age <= most);
            return self.take(0, show_at);
        }
        (None, Picked::Repeated)
    }

    /// How old frames have lately been at most when first there to show —
    /// but for one held up (see [`HELD_UP`]); none counted yet, nothing.
    fn lately(&self) -> f64 {
        let (mut latest, mut next) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &age in &self.ages {
            if age > latest {
                next = latest;
                latest = age;
            } else if age > next {
                next = age;
            }
        }
        let frame = self.period.unwrap_or(1.0 / 60.0);
        if next.is_finite() && latest - next > frame * HELD_UP {
            next
        } else {
            latest.max(0.0)
        }
    }

    /// The delay, given how old frames have lately been when first there to
    /// show: longer at once when that is longer — a frame must not be wanted
    /// before it has come — and shorter only once frames have kept coming
    /// half a frame or more earlier for a while ([`LOWER_AFTER`] at first),
    /// then by a frame a second at most, so an input whose rate drifts from
    /// the show's is not followed down and up again, a frame repeated and
    /// skipped each time. A shortening taken back soon after (see
    /// [`TOO_SOON`]) makes the next wait twice as long, as does one between
    /// late frames that keep coming (see [`RECURRING`]): a camera whose frames
    /// come a frame late now and then keeps the delay that covers them. One
    /// that holds makes it half as long again, a step each [`TOO_SOON`].
    fn follow_delay(&mut self, worst: f64, show_at: f64) -> f64 {
        let frame = self.period.unwrap_or(1.0 / 60.0);
        if let Some(since) = self.held_since {
            if show_at - since >= TOO_SOON {
                self.lower_after = (self.lower_after * 0.5).max(LOWER_AFTER);
                self.held_since = (self.lower_after > LOWER_AFTER).then_some(show_at);
            }
        }
        let delay = match self.delay {
            Some(delay) if worst <= delay => {
                if worst < delay - frame * 0.5 {
                    let since = *self.earlier_since.get_or_insert(show_at);
                    if show_at - since >= self.lower_after {
                        // The next step down a second from now, if frames
                        // still come early enough.
                        self.earlier_since = Some(show_at - self.lower_after + 1.0);
                        self.lowered_at = Some(show_at);
                        self.held_since = Some(show_at);
                        (delay - frame).max(worst)
                    } else {
                        delay
                    }
                } else {
                    self.earlier_since = None;
                    delay
                }
            }
            Some(delay) => {
                self.earlier_since = None;
                if worst > delay + frame * 0.5 {
                    let too_soon = self.lowered_at.is_some_and(|at| show_at - at < TOO_SOON);
                    let recurring = self.raised.is_some_and(|(at, _)| {
                        show_at - at < RECURRING && self.lowered_at.is_some_and(|l| l > at)
                    });
                    if too_soon || recurring {
                        let was = self.raised.map_or(LOWER_AFTER, |(_, wait)| wait);
                        self.lower_after = (self.lower_after.max(was) * 2.0).min(LONGEST_WAIT);
                        self.lowered_at = None;
                    }
                    self.held_since = None;
                    self.raised = Some((show_at, self.lower_after));
                }
                worst
            }
            None => {
                self.earlier_since = None;
                worst
            }
        };
        self.delay = Some(delay);
        delay
    }

    /// Show waiting frame `chosen`, letting go those before it.
    fn take(&mut self, chosen: usize, show_at: f64) -> (Option<F>, Picked) {
        let Some((at, frame)) = self.waiting.drain(..=chosen).next_back() else {
            return (None, Picked::Repeated);
        };
        let skipped =
            if self.shown.is_none() { 0 } else { chosen } + std::mem::take(&mut self.lost);
        self.shown = Some(at);
        self.latency = Some(show_at - at);
        let picked = if skipped == 0 {
            Picked::Next
        } else {
            Picked::Skipped(skipped)
        };
        (Some(frame), picked)
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePicker, Picked};

    /// Counts over a run: show frames that repeated the frame before, input
    /// frames skipped, and the longest run of repeats.
    #[derive(Debug, Default, PartialEq)]
    struct Run {
        repeated: usize,
        skipped: usize,
        repeats_in_a_row: usize,
        /// The most frames skipped by one pick.
        skipped_at_once: usize,
    }

    /// An input capturing every `input_period` seconds, each frame arriving
    /// `latency` later plus `jitter(i)`, shown by a show drawing every
    /// `show_period` seconds, for `secs`: what the picker did, and the
    /// frames shown in order.
    fn run(
        input_period: f64,
        show_period: f64,
        latency: f64,
        jitter: impl Fn(usize) -> f64,
        secs: f64,
    ) -> (Run, Vec<usize>) {
        let mut picker = FramePicker::new();
        let mut arrivals: Vec<(f64, usize, f64)> = (0..((secs + 1.0) / input_period) as usize)
            .map(|i| {
                // Clocks that never line up exactly, and a camera's stamps a
                // little uneven.
                let captured = 1.000_371 + i as f64 * input_period + stamp_jitter(i);
                (captured + latency + jitter(i).max(0.0), i, captured)
            })
            .collect();
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next_arrival = 0;
        let mut run = Run::default();
        let mut shown = Vec::new();
        let mut in_a_row = 0;
        let frames = (secs / show_period) as usize;
        for k in 0..frames {
            let show_at = 1.200_113 + k as f64 * show_period;
            // What has arrived by the time the frame is drawn, a few
            // milliseconds before it is due.
            let drawn = show_at - 0.010;
            while next_arrival < arrivals.len() && arrivals[next_arrival].0 <= drawn {
                let (_, i, captured) = arrivals[next_arrival];
                let _ = picker.push(captured, i);
                next_arrival += 1;
            }
            let (frame, what) = picker.pick(show_at);
            // Settled after the first second.
            let counting = show_at > 2.2;
            match what {
                Picked::Repeated => {
                    in_a_row += 1;
                    if counting {
                        run.repeated += 1;
                        run.repeats_in_a_row = run.repeats_in_a_row.max(in_a_row);
                    }
                }
                Picked::Skipped(n) => {
                    in_a_row = 0;
                    if counting {
                        run.skipped += n;
                        run.skipped_at_once = run.skipped_at_once.max(n);
                    }
                }
                Picked::Next => in_a_row = 0,
            }
            if let Some(i) = frame {
                shown.push(i);
            }
        }
        (run, shown)
    }

    /// An arrival delay that puts frames right around the moment show
    /// frames are drawn: the case where taking the newest to arrive fails.
    const NEAR_THE_DRAW: f64 = 0.036;

    /// A camera's stamps: ±0.3 ms off its clock.
    fn stamp_jitter(i: usize) -> f64 {
        (jitter(0.0006)(i.wrapping_mul(7) + 3)) - 0.0003
    }

    /// Pseudo-random arrival jitter, 0 to `most` seconds, the same each run.
    fn jitter(most: f64) -> impl Fn(usize) -> f64 {
        move |i| {
            let x = (i as u64).wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (x >> 33) as f64 / f64::from(u32::MAX) * most
        }
    }

    /// An input at the show's rate, its frames arriving unevenly, is shown
    /// one frame per show frame: nothing repeated, nothing skipped.
    #[test]
    fn an_input_at_the_shows_rate_moves_one_frame_a_frame_however_unevenly_it_arrives() {
        let (run, shown) = run(1.0 / 60.0, 1.0 / 60.0015, NEAR_THE_DRAW, jitter(0.008), 60.0);
        assert_eq!(run, Run::default(), "{run:?}");
        assert!(shown.windows(2).skip(70).all(|w| w[1] == w[0] + 1));
    }

    /// The same input taken as it arrived: the newest at each show frame. It
    /// repeats and skips frames in pairs — the judder the picker removes.
    #[test]
    fn taken_as_it_arrived_the_same_input_repeats_and_skips() {
        let input = 1.0 / 60.0;
        let show = 1.0 / 60.0015;
        let jitter = jitter(0.008);
        let mut skipped = 0;
        let mut last = None;
        for k in 120..3600 {
            let drawn = 1.200_113 + f64::from(k) * show - 0.010;
            // The newest frame to have arrived by then.
            let newest = (0..100_000)
                .take_while(|&i| 1.000_371 + f64::from(i) * input <= drawn)
                .filter(|&i| {
                    1.000_371 + f64::from(i) * input + NEAR_THE_DRAW + jitter(i as usize) <= drawn
                })
                .last();
            if let (Some(n), Some(l)) = (newest, last) {
                if n > l + 1 {
                    skipped += 1;
                }
            }
            last = newest.or(last);
        }
        assert!(skipped > 20, "latest-arrived skipped only {skipped}");
    }

    /// A camera at 59.94 on a 60 fps show repeats a frame now and then, as
    /// the rates must, one at a time and never skipping.
    #[test]
    fn a_slower_input_repeats_a_frame_now_and_then_one_at_a_time() {
        let (run, _) = run(1.001 / 60.0, 1.0 / 60.0015, NEAR_THE_DRAW, jitter(0.006), 60.0);
        // 60.0015 − 59.94 = 0.06 frames a second: about 3.5 in 59 s.
        assert!((2..=5).contains(&run.repeated), "{run:?}");
        assert_eq!((run.skipped, run.repeats_in_a_row), (0, 1), "{run:?}");
    }

    /// An input a little faster than the show skips a frame now and then,
    /// one at a time and never repeating.
    #[test]
    fn a_faster_input_skips_a_frame_now_and_then_one_at_a_time() {
        let (run, shown) = run(1.0 / 60.1, 1.0 / 60.0, NEAR_THE_DRAW, jitter(0.006), 60.0);
        assert!((4..=8).contains(&run.skipped), "{run:?}");
        assert_eq!(run.repeated, 0, "{run:?}");
        assert!(shown.windows(2).skip(70).all(|w| w[1] - w[0] <= 2));
    }

    /// A 30 fps input on a 60 fps show: every frame shown exactly twice.
    #[test]
    fn a_half_rate_input_shows_each_frame_twice() {
        let (run, shown) = run(1.0 / 29.9993, 1.0 / 60.0015, 0.040, jitter(0.010), 20.0);
        assert_eq!((run.skipped, run.repeats_in_a_row), (0, 1), "{run:?}");
        // 19 s counted: 1140 show frames for 570 input frames.
        assert!((565..=575).contains(&run.repeated), "{run:?}");
        assert!(shown.windows(2).skip(40).all(|w| w[1] == w[0] + 1));
    }

    /// A 50 fps input on a 60 fps show repeats one frame in six, evenly:
    /// never two repeats together.
    #[test]
    fn a_50_fps_input_on_a_60_fps_show_repeats_evenly() {
        let (run, _) = run(1.0 / 50.0, 1.0 / 60.0015, NEAR_THE_DRAW, jitter(0.006), 20.0);
        assert_eq!((run.skipped, run.repeats_in_a_row), (0, 1), "{run:?}");
        assert!((185..=200).contains(&run.repeated), "{run:?}");
    }

    /// However an input's rate drifts from the show's, the delay stays
    /// within a frame and a half of how late its frames come, and nothing
    /// piles up waiting.
    #[test]
    fn the_delay_stays_near_how_late_the_input_comes() {
        for input in [1.0 / 60.1, 1.0 / 59.9, 1.001 / 60.0] {
            let mut picker = FramePicker::new();
            let show = 1.0 / 60.0;
            for k in 0..7200 {
                let show_at = 1.2 + f64::from(k) * show;
                let drawn = show_at - 0.010;
                // Frames captured by then and arrived 36 ms later.
                let mut i = (picker.clock.map_or(1.0, |c| c + input * 0.5) - 1.0) / input;
                loop {
                    let captured = 1.0 + i.ceil() * input;
                    if captured + NEAR_THE_DRAW > drawn {
                        break;
                    }
                    let _ = picker.push(captured, ());
                    i = i.ceil() + 1.0;
                }
                picker.pick(show_at);
            }
            let delay = picker.delay.expect("picking");
            let came = NEAR_THE_DRAW + 0.010;
            assert!(
                delay <= came + 2.5 * input,
                "input {input}: delay {delay} for frames {came} late"
            );
            assert!(picker.waiting.len() <= 3, "{} waiting", picker.waiting.len());
        }
    }

    /// An input that stops for a tenth of a second is shown live again as
    /// soon as it comes back: the frames held up are skipped together, once,
    /// and the delay is not lengthened for them — it once was, and the camera
    /// stayed up to six frames late for ten seconds and more after every
    /// hiccup — then it moves one frame a frame.
    #[test]
    fn an_input_that_stalls_is_shown_live_again_at_once() {
        let stall = |i: usize| if (600..606).contains(&i) { 0.100 - (i - 600) as f64 / 60.0 } else { 0.0 };
        let (run, shown) = run(1.0 / 60.0, 1.0 / 60.0, 0.030, stall, 45.0);
        assert!(run.repeats_in_a_row <= 7, "frozen only while nothing came: {run:?}");
        // Caught up in one go: a delay lengthened for the frames held up
        // would have been caught up later, by single frames.
        assert_eq!(run.skipped, run.skipped_at_once, "{run:?}");
        assert!((4..=7).contains(&run.skipped), "{run:?}");
        let tail = &shown[shown.len() - 600..];
        assert!(tail.windows(2).all(|w| w[1] == w[0] + 1), "even again");
    }

    /// A captured screen showing a still slide sends nothing — macOS sends a
    /// frame only when the picture changes — and a camera can stop for a
    /// moment. When frames come again they are shown at once, not held back
    /// for having been waited for; with the four frames the inputs keep, a
    /// delay grown in the pause once froze the picture for 14–22 s.
    #[test]
    fn frames_after_a_pause_or_a_still_picture_are_shown_at_once() {
        for pause in [0.1, 0.5, 5.0] {
            let input = 1.0 / 60.0;
            let show = 1.0 / 60.0;
            let mut picker = FramePicker::keeping(4);
            let mut captured = 0;
            let mut stopped_at = None;
            let mut first_after: Option<f64> = None;
            for k in 0..1800 {
                let show_at = 1.2 + f64::from(k) * show;
                let drawn = show_at - 0.010;
                // Frames captured until 10 s, none for `pause`, then again;
                // each comes 30 ms after it was captured.
                loop {
                    let at = 1.0 + f64::from(captured) * input;
                    let paused = at > 10.0 && at < 10.0 + pause;
                    if paused {
                        captured += 1;
                        stopped_at.get_or_insert(at);
                        continue;
                    }
                    if at + 0.030 > drawn {
                        break;
                    }
                    let _ = picker.push(at, captured);
                    captured += 1;
                }
                let (frame, _) = picker.pick(show_at);
                if frame.is_some() && show_at > 10.0 + pause + 0.030 {
                    first_after.get_or_insert(show_at);
                }
            }
            // The first frame back comes 30 ms after it was captured, and is
            // shown at the first show frame drawn after that.
            let waited = first_after.expect("frames shown again") - (10.0 + pause);
            assert!(
                waited < 0.030 + 0.010 + 1.0 / 60.0 + 0.001,
                "after a {pause} s pause: shown again {waited:.3} s after it was captured"
            );
        }
    }

    /// Frames captured every `period` until `secs`, each coming
    /// `came(i, captured)` after it was captured, shown on a 60 fps show by a
    /// picker keeping `keep`: for each show frame from `from` on, whether a
    /// new frame was shown, and the latest delay.
    fn kept_run(
        keep: usize,
        period: impl Fn(f64) -> f64,
        came: impl Fn(usize, f64) -> f64,
        secs: f64,
    ) -> (Vec<(f64, Picked)>, FramePicker<usize>) {
        let mut picker = FramePicker::keeping(keep);
        let mut arrivals = Vec::new();
        let mut at = 1.0;
        let mut i = 0;
        while at < secs + 1.0 {
            arrivals.push((at + came(i, at), i, at));
            at += period(at);
            i += 1;
        }
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next = 0;
        let mut picks = Vec::new();
        for k in 0..((secs - 0.2) * 60.0) as usize {
            let show_at = 1.200_113 + k as f64 / 60.0;
            while next < arrivals.len() && arrivals[next].0 <= show_at - 0.010 {
                let (_, i, captured) = arrivals[next];
                let _ = picker.push(captured, i);
                next += 1;
            }
            let (_, what) = picker.pick(show_at);
            picks.push((show_at, what));
        }
        (picks, picker)
    }

    /// However the delay came to be longer than the frames an input keeps
    /// cover — its rate changed, a stamp went wrong — the frames still come
    /// through: wanting older than any frame kept, the oldest is shown and
    /// the delay brought down, rather than every frame let go unshown.
    #[test]
    fn a_delay_longer_than_the_frames_kept_still_lets_them_through() {
        let mut picker = FramePicker::keeping(4);
        let (mut captured, mut in_a_row, mut most) = (0_usize, 0, 0);
        for k in 0..600_u32 {
            let show_at = 1.2 + f64::from(k) / 60.0;
            // Frames every 1/60 s, each coming 30 ms after it was captured.
            while 1.0 + captured as f64 / 60.0 + 0.030 <= show_at - 0.010 {
                let _ = picker.push(1.0 + captured as f64 / 60.0, captured);
                captured += 1;
            }
            if k == 300 {
                picker.ages.extend([0.24, 0.24]);
            }
            let (_, what) = picker.pick(show_at);
            if k > 300 {
                in_a_row = if what == Picked::Repeated { in_a_row + 1 } else { 0 };
                most = most.max(in_a_row);
            }
        }
        assert!(most <= 3, "{most} show frames in a row without a new frame");
    }

    /// A camera gone from 60 fps to 30 is learned at its new rate: its
    /// frames, coming around the moment show frames are drawn, are each shown
    /// twice, none skipped — at the old rate's measure, every one that came a
    /// little late looked held up, and was skipped over.
    #[test]
    fn an_input_that_drops_to_half_rate_is_learned_again() {
        let period = |at: f64| if at < 4.0 { 1.0 / 60.0 } else { 1.0 / 30.0 };
        let came = |i: usize, _: f64| 0.036 + jitter(0.008)(i);
        let (picks, picker) = kept_run(4, period, came, 20.0);
        let late = picks.iter().filter(|(t, _)| (10.0..20.0).contains(t));
        let skipped = late.clone().filter(|(_, p)| matches!(p, Picked::Skipped(_))).count();
        let shown = late.clone().filter(|(_, p)| *p != Picked::Repeated).count();
        assert_eq!(skipped, 0);
        assert!((295..=305).contains(&shown), "{shown} frames shown in 10 s");
        let learned = picker.period.expect("a rate");
        assert!((learned - 1.0 / 30.0).abs() < 0.001, "rate learned {}", 1.0 / learned);
    }

    /// A frame stamped a hair before the one waiting — the same frame again,
    /// its stamp wavering — does not take its place when that would put it
    /// before the frame already shown.
    #[test]
    fn a_wavering_stamp_never_puts_a_frame_before_the_one_shown() {
        let mut picker = FramePicker::keeping(4);
        let frame = 1.0 / 60.0;
        for k in 0..60_u32 {
            let at = 9.0 + f64::from(k) * frame;
            let _ = picker.push(at, k);
            picker.pick(at + 0.030);
        }
        let shown = picker.shown.expect("showing");
        let _ = picker.push(shown + 0.001, 100);
        let _ = picker.push(shown - 0.003, 101);
        let (frame, _) = picker.pick(shown + 0.040);
        assert_eq!(frame, Some(100), "the frame waiting, not one older than the shown");
    }

    /// A camera a hair faster than the show (60.02 fps on 60, as an ATEM
    /// measured), its frames coming around the moment show frames are drawn,
    /// settles on one delay: the delay shortened after ten seconds, then
    /// lengthened by the next late frame, a frame skipped and one repeated
    /// each time — three times a minute on a laptop, 2026-10-04.
    #[test]
    fn a_camera_drifting_past_the_draw_settles_on_one_delay() {
        let input = 1.0 / 60.02;
        let show = 1.0 / 60.0;
        let mut picker = FramePicker::new();
        let jitter = jitter(0.008);
        let mut arrivals: Vec<(f64, usize, f64)> = (0..((310.0 / input) as usize))
            .map(|i| {
                let captured = 1.000_371 + i as f64 * input + stamp_jitter(i);
                (captured + NEAR_THE_DRAW + jitter(i), i, captured)
            })
            .collect();
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut next, mut changes, mut repeats, mut skips) = (0, 0, 0, 0);
        let mut last_delay = None;
        for k in 0..(300.0 / show) as usize {
            let show_at = 1.2 + k as f64 * show;
            while next < arrivals.len() && arrivals[next].0 <= show_at - 0.010 {
                let (_, i, captured) = arrivals[next];
                let _ = picker.push(captured, i);
                next += 1;
            }
            let (_, what) = picker.pick(show_at);
            // The last two minutes, once settled.
            if show_at > 180.0 {
                match what {
                    Picked::Repeated => repeats += 1,
                    Picked::Skipped(n) => skips += n,
                    Picked::Next => {}
                }
                if picker.delay != last_delay && last_delay.is_some() {
                    changes += 1;
                }
            }
            last_delay = picker.delay;
        }
        // 0.02 fps faster: about 2.4 frames over two minutes to skip.
        assert!(skips <= 4, "skips {skips}, repeats {repeats}, delay changes {changes}");
        assert!(repeats <= 1, "skips {skips}, repeats {repeats}, delay changes {changes}");
    }

    /// A camera whose frames come in time but now and then one a frame late
    /// — a USB camera, every 15 s or so — settles on the delay that covers
    /// them: shortened again ten seconds after each, the next late frame cost
    /// a frame repeated, and the shortening one skipped — three times a
    /// minute on a laptop, 2026-10-04. Each shortening that did not hold
    /// waits twice as long before the next.
    #[test]
    fn a_camera_now_and_then_a_frame_late_settles_on_one_delay() {
        let frame = 1.0 / 60.0;
        let mut picker = FramePicker::new();
        let late = |i: usize| if i % 900 == 450 { 0.018 } else { 0.0 };
        // In order, as a USB camera sends them: a late frame holds back the
        // one after it.
        let mut arrived: f64 = 0.0;
        let arrivals: Vec<(f64, usize, f64)> = (0..((310.0 / frame) as usize))
            .map(|i| {
                let captured = 1.000_371 + i as f64 * frame + stamp_jitter(i);
                arrived = (captured + 0.025 + late(i)).max(arrived + 0.001);
                (arrived, i, captured)
            })
            .collect();
        let (mut next, mut repeats, mut skips, mut shortened) = (0, 0, 0, 0);
        let mut last_delay = None::<f64>;
        for k in 0..(300.0 / frame) as usize {
            let show_at = 1.2 + k as f64 * frame;
            while next < arrivals.len() && arrivals[next].0 <= show_at - 0.010 {
                let (_, i, captured) = arrivals[next];
                let _ = picker.push(captured, i);
                next += 1;
            }
            let (_, what) = picker.pick(show_at);
            if show_at > 180.0 {
                match what {
                    Picked::Repeated => repeats += 1,
                    Picked::Skipped(n) => skips += n,
                    Picked::Next => {}
                }
                if let (Some(was), Some(now)) = (last_delay, picker.delay) {
                    if now < was - frame * 0.5 {
                        shortened += 1;
                    }
                }
            }
            last_delay = picker.delay;
        }
        assert!(
            repeats + skips <= 2,
            "the last two minutes: {repeats} repeated, {skips} skipped, shortened {shortened} times"
        );
    }

    /// A camera that came a frame late in a bad patch — each late frame
    /// soon after the delay was shortened again, so that the wait before a
    /// shortening grew to five minutes — and in time since, has its delay
    /// back down within seconds of its next late frame once it has kept
    /// coming in time a few minutes: kept that long, the wait held the frame
    /// of delay a late frame adds on screen five minutes more (codex,
    /// 2026-10-04).
    #[test]
    fn a_delay_raised_after_a_bad_patch_comes_down_soon_once_frames_come_in_time() {
        let frame = 1.0 / 60.0;
        let mut picker = FramePicker::new();
        let captured = |i: usize| 1.000_371 + i as f64 * frame + stamp_jitter(i);
        // Frames captured and on their way, in order: a late one holds back
        // the one after it, as a USB camera sends them.
        let mut coming = std::collections::VecDeque::new();
        let (mut next_capture, mut arrived) = (0_usize, 0.0_f64);
        // From when the next frame captured comes a frame late.
        let mut late_from = Some(3.0);
        let (mut late, mut shortened) = (Vec::new(), Vec::new());
        let mut last_delay = None::<f64>;
        for k in 0_u32.. {
            let show_at = 1.2 + f64::from(k) * frame;
            assert!(show_at < 2_000.0, "late {late:?}, shortened {shortened:?}");
            while captured(next_capture) <= show_at {
                let at = captured(next_capture);
                let is_late = late_from.is_some_and(|from| at >= from);
                if is_late {
                    late_from = None;
                    late.push(at);
                }
                arrived = (at + 0.025 + if is_late { 0.018 } else { 0.0 }).max(arrived + 0.001);
                coming.push_back((arrived, next_capture, at));
                next_capture += 1;
            }
            while coming.front().is_some_and(|&(when, _, _)| when <= show_at - 0.010) {
                let (_, i, at) = coming.pop_front().unwrap();
                let _ = picker.push(at, i);
            }
            let _ = picker.pick(show_at);
            if let (Some(was), Some(now)) = (last_delay, picker.delay) {
                if now < was - frame * 0.5 {
                    shortened.push(show_at);
                    match late.len() {
                        // The bad patch: a late frame 20 s after each
                        // shortening, which was too soon.
                        ..6 => late_from = Some(show_at + 20.0),
                        // In time since, but for one late frame 5½ minutes on.
                        6 => late_from = Some(show_at + 330.0),
                        _ => break,
                    }
                }
            }
            last_delay = picker.delay;
        }
        let waited = |n: usize| shortened[n] - late[n];
        assert!(waited(5) > 300.0, "the bad patch: waited {:.0} s", waited(5));
        assert!(waited(6) < 15.0, "in time since: waited {:.0} s", waited(6));
    }

    /// Set to the lowest latency, a camera's newest frame is shown at each
    /// pick, the ones before it skipped, and a pick with none new repeats
    /// the frame before; turned off again, frames are picked at the delay
    /// that keeps them even, followed all the while.
    #[test]
    fn at_the_lowest_latency_the_newest_frame_is_shown_each_pick() {
        let frame = 1.0 / 60.0;
        let mut picker = FramePicker::new();
        // Steady frames, 25 ms on their way, picked 10 ms before each show
        // frame: at the even delay, a frame held behind each newest.
        for i in 0..240_u32 {
            let at = 1.0 + f64::from(i) * frame;
            let _ = picker.push(at, i);
            let _ = picker.pick(at + 0.045);
        }
        let held = picker.latency().expect("a latency");
        picker.set_newest(true);
        // Two come at once: the newer shown, the one before it skipped.
        let at = 1.0 + 240.0 * frame;
        let _ = picker.push(at, 240);
        let _ = picker.push(at + frame, 241);
        assert_eq!(picker.pick(at + frame + 0.030), (Some(241), Picked::Skipped(1)));
        assert!(picker.latency().expect("a latency") < held, "sooner");
        // None new: the frame before again.
        assert_eq!(picker.pick(at + 2.0 * frame + 0.030), (None, Picked::Repeated));
        // Off again: each in turn.
        picker.set_newest(false);
        for i in 242..300_u32 {
            let at = 1.0 + f64::from(i) * frame;
            let _ = picker.push(at, i);
            let (_, what) = picker.pick(at + 0.045);
            assert_ne!(what, Picked::Skipped(1), "frame {i}");
        }
    }

    /// A camera a frame late every 80 s — too seldom for a shortening to be
    /// taken back within [`super::TOO_SOON`] — settles on the delay that
    /// covers it all the same: shortened 12 s after each late frame and raised
    /// again at the next, it skipped a frame and repeated one each time, for
    /// as long as it ran (codex, 2026-10-04).
    #[test]
    fn a_camera_a_frame_late_every_80_s_settles_on_one_delay() {
        let frame = 1.0 / 60.0;
        let mut picker = FramePicker::new();
        let late = |i: usize| if i % 4_800 == 180 { 0.018 } else { 0.0 };
        let mut arrived: f64 = 0.0;
        let arrivals: Vec<(f64, usize, f64)> = (0..((910.0 / frame) as usize))
            .map(|i| {
                let captured = 1.000_371 + i as f64 * frame + stamp_jitter(i);
                arrived = (captured + 0.025 + late(i)).max(arrived + 0.001);
                (arrived, i, captured)
            })
            .collect();
        let (mut next, mut repeats, mut skips) = (0, 0, 0);
        for k in 0..(900.0 / frame) as usize {
            let show_at = 1.2 + k as f64 * frame;
            while next < arrivals.len() && arrivals[next].0 <= show_at - 0.010 {
                let (_, i, captured) = arrivals[next];
                let _ = picker.push(captured, i);
                next += 1;
            }
            let (_, what) = picker.pick(show_at);
            if show_at > 500.0 {
                match what {
                    Picked::Repeated => repeats += 1,
                    Picked::Skipped(n) => skips += n,
                    Picked::Next => {}
                }
            }
        }
        assert_eq!((repeats, skips), (0, 0), "the last 400 s");
    }

    /// Show frames timed by refresh calls that reach Nida up to 2 ms late
    /// — as the frame trace showed them — pick an input at the show's rate
    /// one frame a frame: its frames sat right where a frame is wanted, and
    /// the wobble had the picker show one twice and skip the next, up to
    /// seven times a second (a laptop, 2026-10-04).
    #[test]
    fn show_frames_timed_a_little_late_still_pick_one_frame_a_frame() {
        let frame = 1.0 / 60.0;
        let mut picker = FramePicker::keeping(4);
        // Up to 3 ms (the helper gives up to half the figure it is given).
        let late = jitter(0.006);
        // Frames come 12 ms after they are captured, one in two seconds a
        // frame later — which holds the delay up, so frames wait to be
        // shown, two at a time, and which of them is wanted is close.
        // In order, as a USB camera sends them.
        let mut arrived: f64 = 0.0;
        let arrivals: Vec<f64> = (0..4000_usize)
            .map(|i| {
                let captured = 1.0 + i as f64 * frame;
                let came = captured + 0.012 + if i % 120 == 60 { 0.018 } else { 0.0 };
                arrived = came.max(arrived + 0.0005);
                arrived
            })
            .collect();
        let (mut captured, mut repeats, mut skips) = (0_usize, 0, 0);
        for k in 0..3600_usize {
            // The show's refresh, and when its call reached Nida.
            let refresh = 1.2 + k as f64 * frame;
            let show_at = refresh + late(k + 7);
            while arrivals[captured] <= refresh - 0.010 {
                let _ = picker.push(1.0 + captured as f64 * frame, captured);
                captured += 1;
            }
            let (_, what) = picker.pick(show_at);
            if k > 120 {
                match what {
                    Picked::Repeated => repeats += 1,
                    Picked::Skipped(n) => skips += n,
                    Picked::Next => {}
                }
            }
        }
        assert_eq!((repeats, skips), (0, 0), "a minute of show frames");
    }

    /// A camera drifting from the show by a frame in three minutes — faster
    /// or slower — its frames timed against show frames that wobble 3 ms: as
    /// its frames pass the mark between two show frames, one frame is skipped
    /// or shown twice, once — not both by turns for as long as the wobble
    /// straddles the mark.
    #[test]
    fn a_camera_drifting_slowly_past_the_mark_skips_or_repeats_once() {
        let frame = 1.0 / 60.0;
        for rate in [1.0001, 0.9999] {
            let input = frame / rate;
            let mut picker = FramePicker::keeping(4);
            let late = jitter(0.006);
            let (mut captured, mut repeats, mut skips, mut pairs) = (0_usize, 0, 0, 0);
            let mut last_turn: Option<(usize, bool)> = None;
            for k in 0..(240.0 / frame) as usize {
                let refresh = 1.2 + k as f64 * frame;
                let show_at = refresh + late(k + 3);
                while 1.0 + captured as f64 * input + 0.012 <= refresh - 0.010 {
                    let _ = picker.push(1.0 + captured as f64 * input, captured);
                    captured += 1;
                }
                let (_, what) = picker.pick(show_at);
                if k < 120 {
                    continue;
                }
                let turn = match what {
                    Picked::Repeated => {
                        repeats += 1;
                        Some(false)
                    }
                    Picked::Skipped(n) => {
                        skips += n;
                        Some(true)
                    }
                    Picked::Next => None,
                };
                if let Some(skipped) = turn {
                    if let Some((at, was_skip)) = last_turn {
                        if k - at <= 3 && was_skip != skipped {
                            pairs += 1;
                        }
                    }
                    last_turn = Some((k, skipped));
                }
            }
            assert_eq!(pairs, 0, "rate {rate}: {repeats} repeated, {skips} skipped");
            assert!(repeats + skips <= 3, "rate {rate}: {repeats} repeated, {skips} skipped");
        }
    }

    /// A capture clock that goes back (a restarted stream) starts the
    /// picking afresh: frames still go in order, and a frame older than
    /// the one shown is never shown after it.
    #[test]
    fn a_clock_that_goes_back_starts_afresh_in_order() {
        let mut picker = FramePicker::keeping(4);
        let frame = 1.0 / 60.0;
        let mut shown = Vec::new();
        for k in 0..300_u32 {
            // Captured on a clock that jumps back 5 s at frame 150.
            let at = if k < 150 { 100.0 } else { 95.0 } + f64::from(k) * frame;
            let _ = picker.push(at, k);
            let show_at = at + 0.040;
            if let (Some(k), _) = picker.pick(show_at) {
                shown.push(k);
            }
        }
        assert!(shown.windows(2).all(|w| w[1] > w[0]), "in order: {shown:?}");
        assert!(shown.iter().any(|&k| k > 160), "picked again after the jump");
    }

    /// Frames that always come in time are shown as soon as they come: no
    /// later than the newest to have arrived — the delay this first added
    /// for nothing ("bigger delay then before").
    #[test]
    fn frames_that_come_in_time_are_shown_no_later_than_as_they_come() {
        let input = 1.0 / 60.0;
        let show = 1.0 / 60.0015;
        // Frames come 20–22 ms after capture: always well before the draw.
        let came = |i: usize| 0.020 + jitter(0.002)(i);
        let mut picker = FramePicker::new();
        let (mut picked_age, mut newest_age, mut n) = (0.0, 0.0, 0.0);
        let mut pushed = 0;
        for k in 0..1800 {
            let show_at = 1.200_113 + f64::from(k) * show;
            let drawn = show_at - 0.010;
            while 1.000_371 + pushed as f64 * input + came(pushed) <= drawn {
                let _ = picker.push(1.000_371 + pushed as f64 * input, pushed);
                pushed += 1;
            }
            let newest_captured = 1.000_371 + (pushed - 1) as f64 * input;
            if picker.pick(show_at).0.is_some() && k > 120 {
                picked_age += picker.latency().expect("picked");
                newest_age += show_at - newest_captured;
                n += 1.0;
            }
        }
        let extra = (picked_age - newest_age) / n;
        assert!(extra.abs() < 0.001, "{:.2} ms later", extra * 1000.0);
    }
}


