//! The stream's bit rate following what its upload carries. A destination
//! falling behind gets the rate cut; once every destination keeps up with
//! room to spare it goes back up, a step at a time. Before, an upload that
//! could not carry the stream lost parts of it — a picture that broke up
//! until the next keyframe — until the operator stopped and lowered the rate
//! by hand.
//!
//! What it goes by is the stream waiting at each destination, as a queue in
//! the network is judged (`CoDel`): not its peaks — an upload at its limit
//! empties in bursts, and the queue swings between nothing and a few hundred
//! kilobytes in a second — but the least it held over the last two seconds.
//! Stream still waiting at its lowest is stream the upload is not carrying.
//!
//! One encoder serves every destination, so the rate follows the slowest of
//! those sending; one still connecting, waiting to reconnect, or only just
//! connected is not waited for.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// One destination as the controller sees it, at each look.
#[derive(Debug, Clone, Copy, Default)]
pub struct Seen {
    /// Bytes of stream waiting to be sent.
    pub queued: usize,
    /// Pieces of stream thrown away so far, as a running count.
    pub dropped: u64,
    /// The rate going out over the last few seconds, in kbit/s; 0 while not
    /// known.
    pub kbps: f32,
    /// Connected and sending.
    pub sending: bool,
    /// Seconds since it connected.
    pub live_secs: f32,
    /// The connection's number: a new one is a new destination.
    pub generation: u64,
    /// The count of network changes when the connection began (see
    /// `streaming::network`): a connection begun at a higher one than the
    /// one before it went out a new way.
    pub network: u64,
}

/// The window the waiting is judged over.
const WINDOW: Duration = Duration::from_secs(2);

/// Stream still waiting at its lowest over [`WINDOW`], in seconds at the rate
/// asked for, past which the upload is not carrying it. It builds only once
/// the relay's pipe and the connection's own buffers are full, so it is
/// already late; a destination loses pieces only seconds after
/// (`buffer_secs`).
const BEHIND_SECS: f64 = 0.75;

/// Waiting over [`WINDOW`] — half the time or more — under this, the upload
/// has room: one at its limit holds some most of the time, while one with
/// room only now and then, in a burst (Wi-Fi, a phone's 4G).
const CLEAR_SECS: f64 = 0.15;

/// Time between cuts: the media engine follows a new rate within about a
/// second (measured), and the queue then drains.
const CUT_HOLD: Duration = Duration::from_millis(1500);

/// After a cut, the waiting must drain — by a twentieth at its lowest each
/// time this passes — or the cut was not enough: the rate the upload was
/// read to carry was more than it does (a reading of a connection's sending,
/// its buffers filling, runs high), or it drained part way and then held.
const DRAIN_GRACE: Duration = Duration::from_secs(4);

/// Time the upload has room before the rate goes up a step.
const RAISE_AFTER: Duration = Duration::from_secs(6);

/// Near the rate that last proved too much — within this share of it — the
/// rate goes up in small steps, and half as often: the upload was full there.
const NEAR_CEILING: f64 = 0.85;

/// A destination connected this long has figures to go by: its rate going
/// out is measured over four seconds.
const WARM_SECS: f32 = 4.0;

/// A cut takes the rate to this share of what it was, at most.
const CUT: f64 = 0.7;

/// Of what the slowest destination gets out, the share a cut leaves the
/// video — the rest is room for the queue to drain.
const HEADROOM: f64 = 0.9;

/// The least video rate worth sending a `width` × `height` picture at `fps`:
/// about 0.004 bits a pixel — a picture past use, but whole — and never under
/// 300 kbit/s. A 1080p50 stream goes down to 414, a 4K60 one to 1990.
pub fn floor_kbps(width: u32, height: u32, fps: u32) -> u32 {
    let pixels = u64::from(width) * u64::from(height) * u64::from(fps.max(1));
    u32::try_from(pixels * 4 / 1_000_000).unwrap_or(u32::MAX).max(300)
}

/// One destination's figures between looks.
#[derive(Debug, Clone)]
struct Known {
    generation: u64,
    network: u64,
    dropped: u64,
    /// The waiting when the rate was last cut, and how it has drained since:
    /// waiting that keeps draining wants no further cut.
    at_cut: Option<CutMark>,
    /// Bytes waiting at recent looks, a tenth of a second apart at most.
    history: VecDeque<(Instant, usize)>,
}

/// The waiting at a cut, at its lowest over the window, and where it was
/// at the last check on its draining.
#[derive(Debug, Clone, Copy)]
struct CutMark {
    level: usize,
    checked: Instant,
    checked_level: usize,
}

/// The least waiting over the last [`WINDOW`], and the middle of it.
#[derive(Debug, Clone, Copy)]
struct Span {
    least: usize,
    median: usize,
}

impl Known {
    fn new(generation: u64, network: u64, dropped: u64) -> Self {
        Self {
            generation,
            network,
            dropped,
            at_cut: None,
            history: VecDeque::new(),
        }
    }

    /// Note `queued` now: the least and most waiting over the last window —
    /// `None` until a whole window's figures are in.
    fn note(&mut self, now: Instant, queued: usize) -> Option<Span> {
        if self
            .history
            .back()
            .is_none_or(|(at, _)| now.duration_since(*at) >= Duration::from_millis(100))
        {
            self.history.push_back((now, queued));
        }
        // Kept back to the newest look a whole window old.
        while self.history.len() > 1
            && self
                .history
                .get(1)
                .is_some_and(|(at, _)| now.duration_since(*at) >= WINDOW)
        {
            self.history.pop_front();
        }
        let (oldest, _) = *self.history.front()?;
        if now.duration_since(oldest) < WINDOW {
            return None;
        }
        let mut waiting: Vec<usize> = self
            .history
            .iter()
            .map(|&(_, q)| q)
            .chain([queued])
            .collect();
        waiting.sort_unstable();
        Some(Span {
            least: waiting[0],
            median: waiting[waiting.len() / 2],
        })
    }
}

/// The video rate a stream asks of its encoder, following its upload.
#[derive(Debug, Clone)]
pub struct RateControl {
    /// The rate the operator set: never more.
    asked: u32,
    /// The sound's rate, which goes out beside the video unchanged.
    audio: u32,
    target: u32,
    /// Never less (see [`floor_kbps`]): what the picture still shows, or the
    /// rate asked for when that is lower.
    floor: u32,
    /// What the picture still shows.
    least: u32,
    /// Bytes a destination may hold before it loses pieces.
    limit: usize,
    last_cut: Option<Instant>,
    /// The rate the upload last proved unable to carry: it is neared slowly.
    ceiling: Option<u32>,
    /// Since when every destination has had room.
    clear_since: Option<Instant>,
    /// The rate at the last look every destination had room at: what the
    /// upload carried before any trouble — not a rate held between cuts.
    steady: u32,
    known: Vec<Option<Known>>,
}

impl RateControl {
    /// For a stream of `asked` kbit/s of video and `audio` of sound, cut no
    /// lower than `floor`, each destination holding up to `limit` bytes.
    pub fn new(asked: u32, audio: u32, floor: u32, limit: usize) -> Self {
        Self {
            asked,
            audio,
            target: asked,
            floor: floor.min(asked),
            least: floor,
            limit,
            last_cut: None,
            ceiling: None,
            clear_since: None,
            steady: asked,
            known: Vec::new(),
        }
    }

    /// The video rate to ask for now.
    pub fn target(&self) -> u32 {
        self.target
    }

    /// The rate the operator set.
    pub fn asked(&self) -> u32 {
        self.asked
    }

    /// The least it goes to.
    pub fn floor(&self) -> u32 {
        self.floor
    }

    /// The operator set `asked` while live, each destination now holding up
    /// to `limit` bytes: never more than it from now on. A rate at the one
    /// set before follows the new one at once, down or up; one the upload
    /// holds down stays down — under the new one — and climbs to it as
    /// before.
    pub fn set_asked(&mut self, asked: u32, limit: usize) {
        let following = self.target >= self.asked;
        self.asked = asked;
        self.floor = self.least.min(asked);
        self.limit = limit;
        let target = if following {
            asked
        } else {
            self.target.min(asked)
        };
        self.target = target.max(self.floor);
        // Room to go up is judged afresh at the new rate.
        self.clear_since = None;
    }

    /// Whether the rate asked for is as low as it goes.
    #[cfg(test)]
    fn at_floor(&self) -> bool {
        self.target <= self.floor
    }

    /// Look at the destinations `seen` now: the video rate to ask for.
    pub fn look(&mut self, now: Instant, seen: &[Seen]) -> u32 {
        self.known.resize(seen.len(), None);
        let bytes_per_sec = f64::from(self.target + self.audio) * 1000.0 / 8.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Small, positive.
        let bytes_of = |secs: f64| (secs * bytes_per_sec) as usize;
        let (behind, clear) = (bytes_of(BEHIND_SECS), bytes_of(CLEAR_SECS));

        let mut lost = false;
        let mut standing = false;
        let mut imminent = false;
        let mut taking_part = 0;
        let mut all_room = true;
        let mut new_way = false;
        // Which destinations have stream waiting at their least over a whole
        // window now: only theirs says what an upload carries (see `cut`).
        let mut waiting = vec![false; seen.len()];
        for ((s, known), waits) in seen.iter().zip(&mut self.known).zip(&mut waiting) {
            // A new connection: what was known of the one before is not its.
            // Made after the network changed, it goes out a new way.
            if known
                .as_ref()
                .is_none_or(|k| k.generation != s.generation)
            {
                // Its first connection — switched on, or the stream just
                // started — went out no new way.
                new_way |= known
                    .as_ref()
                    .is_some_and(|k| k.generation != 0 && s.network > k.network);
                *known = Some(Known::new(s.generation, s.network, s.dropped));
            }
            let Some(k) = known.as_mut() else { continue };
            let new_loss = s.dropped > k.dropped;
            k.dropped = s.dropped;
            let span = k.note(now, s.queued);
            if !s.sending || s.live_secs < WARM_SECS {
                continue;
            }
            taking_part += 1;
            lost |= new_loss;
            imminent |= s.queued >= self.limit / 2;
            let Some(span) = span else {
                // Not a window's figures yet: nothing to go by but loss.
                all_room = false;
                continue;
            };
            // Waiting at its lowest past the mark, and either more than at
            // the last cut by a tenth or not draining since it: the upload is
            // not carrying the stream, and the cut before did not do it.
            let past_cut = match k.at_cut.as_mut() {
                None => true,
                Some(mark) if span.least > mark.level + mark.level / 10 => true,
                Some(mark) if now.duration_since(mark.checked) >= DRAIN_GRACE => {
                    let draining = span.least + mark.checked_level / 20 < mark.checked_level;
                    if draining {
                        mark.checked = now;
                        mark.checked_level = span.least;
                    }
                    !draining
                }
                Some(_) => false,
            };
            *waits = span.least > behind;
            standing |= span.least > behind && past_cut;
            if span.median < clear {
                k.at_cut = None;
            } else {
                all_room = false;
            }
        }

        if new_way {
            // What was cut while the old way died was cut for nothing: back
            // to the rate the upload carried before, and from there as usual.
            if self.steady > self.target {
                log::info!(
                    "Stream video bit rate back to {} kbps: reconnected over a new network",
                    self.steady
                );
            }
            self.target = self.target.max(self.steady).min(self.asked);
            self.ceiling = None;
            self.clear_since = None;
            self.last_cut = Some(now);
            return self.target;
        }
        if lost || standing || imminent {
            self.clear_since = None;
            let cut_due = self
                .last_cut
                .is_none_or(|at| now.duration_since(at) >= CUT_HOLD);
            if cut_due && self.target > self.floor {
                self.cut(now, seen, &waiting);
            }
        } else if taking_part > 0 && all_room {
            // A rate the upload carried — unless a destination switched on
            // is not sending: its way may be the one that failed, and the
            // rate before its trouble is the one to go back to.
            let every_one = seen
                .iter()
                .all(|s| s.generation == 0 || (s.sending && s.live_secs >= WARM_SECS));
            if every_one {
                self.steady = self.target;
            }
            let since = *self.clear_since.get_or_insert(now);
            // Near the rate that last proved too much, small steps, half as
            // often; past it, the upload carries more than it did then.
            let mut step = (self.asked / 10).max(200);
            let mut wait = RAISE_AFTER;
            if let Some(ceiling) = self.ceiling {
                if f64::from(self.target + step) > f64::from(ceiling) * NEAR_CEILING {
                    step = (self.asked / 40).max(100);
                    wait = RAISE_AFTER * 2;
                }
            }
            if now.duration_since(since) >= wait && self.target < self.asked {
                self.target = (self.target + step).min(self.asked);
                self.clear_since = Some(now);
                if self.ceiling.is_some_and(|c| self.target > c) {
                    self.ceiling = None;
                }
            }
        } else {
            // Waiting but draining, the upload at its limit, or no
            // destination to go by: held, and having room starts over.
            self.clear_since = None;
        }
        self.target
    }

    /// Cut the rate: to [`CUT`] of what it is, or under what the slowest
    /// destination with stream waiting gets out when that is lower.
    fn cut(&mut self, now: Instant, seen: &[Seen], waiting: &[bool]) {
        self.ceiling = Some(self.target);
        let mut next = f64::from(self.target) * CUT;
        // What a destination gets out says what its upload carries only
        // while stream waits for it — over a whole window, at its least
        // (`waiting`): one keeping up sends what the picture needs — a still
        // slide's few hundred kilobits held Variable — not what its upload
        // could.
        let slowest = seen
            .iter()
            .zip(waiting)
            .filter(|(s, &waits)| s.sending && s.live_secs >= WARM_SECS && s.kbps > 0.0 && waits)
            .map(|(s, _)| f64::from(s.kbps))
            .fold(f64::INFINITY, f64::min);
        if slowest.is_finite() {
            next = next.min((slowest - f64::from(self.audio)) * HEADROOM);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped.
        let next = next.max(0.0) as u32;
        self.target = next.clamp(self.floor, self.asked);
        self.last_cut = Some(now);
        for known in self.known.iter_mut().flatten() {
            if let Some(least) = known.history.iter().map(|&(_, q)| q).min() {
                known.at_cut = Some(CutMark {
                    level: least,
                    checked: now,
                    checked_level: least,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 10_000_000;

    fn sending(queued: usize, dropped: u64, kbps: f32) -> Seen {
        Seen {
            queued,
            dropped,
            kbps,
            sending: true,
            live_secs: 30.0,
            generation: 1,
            network: 0,
        }
    }

    /// Bytes waiting for `secs` of a stream of `kbps`.
    fn secs_of(secs: f64, kbps: u32) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bytes = (secs * f64::from(kbps) * 1000.0 / 8.0) as usize;
        bytes
    }

    fn control() -> RateControl {
        RateControl::new(6000, 160, 415, LIMIT)
    }

    /// Look every tenth of a second for `secs`, at `seen(t)` for `t` the
    /// seconds since `from`: the time after.
    fn looks(
        rate: &mut RateControl,
        from: Instant,
        secs: f64,
        mut seen: impl FnMut(f64) -> Vec<Seen>,
    ) -> Instant {
        let mut t = 0.0;
        while t < secs {
            rate.look(from + Duration::from_secs_f64(t), &seen(t));
            t += 0.1;
        }
        from + Duration::from_secs_f64(t)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn bytes(v: f64) -> usize {
        v as usize
    }

    /// The floor goes with the picture: 0.004 bits a pixel, at least 300.
    #[test]
    fn the_floor_goes_with_the_picture() {
        assert_eq!(floor_kbps(1920, 1080, 50), 414);
        assert_eq!(floor_kbps(3840, 2160, 60), 1990);
        assert_eq!(floor_kbps(1280, 720, 30), 300);
    }

    /// Stream left waiting for two seconds gets the rate cut — not before,
    /// and no sooner after than the engine can follow — and a queue that
    /// keeps growing gets it cut again, down to the floor and no further.
    #[test]
    fn a_standing_queue_cuts_the_rate_to_the_floor() {
        let mut rate = control();
        let start = Instant::now();
        let waiting = |_| vec![sending(secs_of(1.0, 6160), 0, 0.0)];
        let then = looks(&mut rate, start, 1.9, waiting);
        assert_eq!(rate.target(), 6000, "not before two seconds of it");
        let then = looks(&mut rate, then, 0.3, waiting);
        assert_eq!(rate.target(), 4200, "cut");
        // Growing on, by a fifth of a second's stream every tenth.
        let grown = |t: f64| vec![sending(secs_of(1.0 + t * 2.0, 6160), 0, 0.0)];
        looks(&mut rate, then, 60.0, grown);
        assert_eq!(rate.target(), 415);
        assert!(rate.at_floor());
    }

    /// A queue that swings between nothing and more than the mark — an
    /// upload at its limit, emptying in bursts — is held, neither cut nor
    /// raised.
    #[test]
    fn a_queue_swinging_at_the_limit_holds_the_rate() {
        let mut rate = control();
        // Holding stream seven tenths of the time, empty the rest.
        let burst = |t: f64| {
            let tenth = bytes((t * 10.0).round()) % 10;
            let queued = if tenth < 7 { secs_of(1.5, 6160) } else { 0 };
            vec![sending(queued, 0, 6000.0)]
        };
        looks(&mut rate, Instant::now(), 30.0, burst);
        assert_eq!(rate.target(), 6000);
    }

    /// After a cut, a queue draining — however long it still takes at the
    /// lower rate — wants no further cut; one growing again past where it
    /// was does.
    #[test]
    fn a_queue_draining_after_a_cut_is_left_to_drain() {
        let mut rate = control();
        let at_cut = secs_of(2.0, 6160);
        let now = looks(&mut rate, Instant::now(), 2.5, |_| vec![sending(at_cut, 0, 0.0)]);
        assert_eq!(rate.target(), 4200);
        let draining = |t: f64| vec![sending(at_cut - bytes(t * 40_000.0), 0, 0.0)];
        let now = looks(&mut rate, now, 10.0, draining);
        assert_eq!(rate.target(), 4200, "draining");
        let low = at_cut - 400_000;
        let growing = |t: f64| vec![sending(low + bytes(t * 100_000.0), 0, 0.0)];
        looks(&mut rate, now, 6.0, growing);
        assert!(rate.target() <= 2940, "growing again past where it was cut: {}", rate.target());
    }

    /// A cut after which the waiting does not drain at all — the upload read
    /// to carry more than it does — is followed by another.
    #[test]
    fn a_cut_that_does_not_drain_is_followed_by_another() {
        let mut rate = control();
        let stuck = secs_of(2.0, 6160);
        let now = looks(&mut rate, Instant::now(), 2.5, |_| vec![sending(stuck, 0, 0.0)]);
        assert_eq!(rate.target(), 4200);
        let now = looks(&mut rate, now, 3.0, |_| vec![sending(stuck, 0, 0.0)]);
        assert_eq!(rate.target(), 4200, "given time to drain");
        looks(&mut rate, now, 2.0, |_| vec![sending(stuck, 0, 0.0)]);
        assert_eq!(rate.target(), 2940, "it did not: cut again");
    }

    /// A queue that drains part way after a cut and then holds is cut again.
    #[test]
    fn a_queue_that_stops_draining_is_cut_again() {
        let mut rate = control();
        let at_cut = secs_of(2.0, 6160);
        let now = looks(&mut rate, Instant::now(), 2.5, |_| vec![sending(at_cut, 0, 0.0)]);
        assert_eq!(rate.target(), 4200);
        let held = at_cut - 400_000;
        let part_way = |t: f64| {
            let drained = bytes((t * 200_000.0).min(400_000.0));
            vec![sending(at_cut - drained, 0, 0.0)]
        };
        let now = looks(&mut rate, now, 5.0, part_way);
        assert_eq!(rate.target(), 4200, "draining");
        looks(&mut rate, now, 5.0, |_| vec![sending(held, 0, 0.0)]);
        assert_eq!(rate.target(), 2940, "it stopped: cut again");
    }

    /// A link with room, bursting now and then — a phone's 4G — lets the
    /// rate go back up: most of the time nothing waits.
    #[test]
    fn a_bursty_link_with_room_still_lets_the_rate_up() {
        let mut rate = control();
        let behind = |_| vec![sending(secs_of(2.0, 6160), 0, 0.0)];
        let now = looks(&mut rate, Instant::now(), 2.5, behind);
        assert_eq!(rate.target(), 4200);
        // A burst two tenths in every ten.
        let bursty = |t: f64| {
            let tenth = bytes((t * 10.0).round()) % 10;
            let queued = if tenth < 2 { secs_of(0.5, 4360) } else { 0 };
            vec![sending(queued, 0, 4000.0)]
        };
        looks(&mut rate, now, 12.0, bursty);
        assert!(rate.target() > 4200, "raised: {}", rate.target());
    }

    /// A piece thrown away cuts the rate though nothing waits now.
    #[test]
    fn a_piece_lost_cuts_the_rate() {
        let mut rate = control();
        let now = Instant::now();
        assert_eq!(rate.look(now, &[sending(0, 3, 0.0)]), 6000, "counted, not new");
        assert_eq!(rate.look(now + Duration::from_secs(1), &[sending(0, 4, 0.0)]), 4200);
    }

    /// Half the bytes a destination may hold waiting: cut at once.
    #[test]
    fn a_queue_near_overflow_cuts_the_rate() {
        let mut rate = control();
        assert_eq!(rate.look(Instant::now(), &[sending(LIMIT / 2, 0, 0.0)]), 4200);
    }

    /// A cut goes to what the slowest destination gets out, less the sound,
    /// with room to drain, when that is lower than the usual cut.
    #[test]
    fn a_cut_follows_what_the_slowest_gets_out() {
        let mut rate = control();
        let seen = vec![sending(0, 0, 6100.0), sending(secs_of(1.0, 6160), 0, 2160.0)];
        looks(&mut rate, Instant::now(), 2.5, |_| seen.clone());
        assert_eq!(rate.target(), 1800);
    }

    /// With room for long enough, the rate goes back up a step at a time, to
    /// the rate asked for and no further — in small steps, half as often,
    /// near the rate that last proved too much.
    #[test]
    fn the_rate_climbs_back_slowly_near_where_it_was_too_much() {
        let mut rate = RateControl::new(3000, 160, 300, LIMIT);
        let behind = |_| vec![sending(secs_of(2.0, 3160), 0, 0.0)];
        let now = looks(&mut rate, Instant::now(), 2.5, behind);
        assert_eq!(rate.target(), 2100);
        let room = |_| vec![sending(0, 0, 2000.0)];
        // Two seconds to see the room, then six to raise a full step.
        let now = looks(&mut rate, now, 8.5, room);
        assert_eq!(rate.target(), 2400, "a full step, well under where it was too much");
        let now = looks(&mut rate, now, 6.5, room);
        assert_eq!(rate.target(), 2400, "near it: not yet");
        let now = looks(&mut rate, now, 6.0, room);
        assert_eq!(rate.target(), 2500, "a small step, after twice as long");
        looks(&mut rate, now, 200.0, room);
        assert_eq!(rate.target(), 3000, "up to the rate asked for and no further");
    }

    /// With no destination to go by — none sending, or each only just
    /// connected — the rate is neither cut nor raised.
    #[test]
    fn no_destination_to_go_by_holds_the_rate() {
        let mut rate = control();
        let behind = |_| vec![sending(secs_of(2.0, 6160), 0, 0.0)];
        let now = looks(&mut rate, Instant::now(), 2.5, behind);
        assert_eq!(rate.target(), 4200);
        let just_connected = Seen {
            live_secs: 1.0,
            generation: 2,
            ..sending(secs_of(3.0, 6160), 0, 0.0)
        };
        let now = looks(&mut rate, now, 30.0, |_| vec![just_connected]);
        assert_eq!(rate.target(), 4200);
        let reconnecting = Seen {
            sending: false,
            generation: 3,
            ..sending(0, 0, 0.0)
        };
        looks(&mut rate, now, 30.0, |_| vec![reconnecting]);
        assert_eq!(rate.target(), 4200);
    }

    /// A destination reconnected is a new one: its count of pieces lost
    /// starts again from what it says, and is not taken for a new loss.
    #[test]
    fn a_reconnected_destination_starts_afresh() {
        let mut rate = control();
        let now = Instant::now();
        rate.look(now, &[sending(0, 9, 6100.0)]);
        let again = Seen {
            generation: 2,
            ..sending(0, 2, 6100.0)
        };
        assert_eq!(rate.look(now + Duration::from_secs(1), &[again]), 6000);
        let more = Seen {
            generation: 2,
            ..sending(0, 3, 6100.0)
        };
        assert_eq!(rate.look(now + Duration::from_secs(2), &[more]), 4200, "a loss on it now");
    }

    /// A rate set while live: at the rate set before, the rate follows it at
    /// once, down or up; held down by the upload, it stays down, under the
    /// new rate, and climbs to it; set under the picture's floor, it is its
    /// own floor.
    #[test]
    fn a_rate_set_while_live_is_followed() {
        let mut rate = control();
        rate.set_asked(3000, LIMIT);
        assert_eq!((rate.asked(), rate.target()), (3000, 3000), "down at once");
        rate.set_asked(8000, LIMIT);
        assert_eq!((rate.asked(), rate.target()), (8000, 8000), "up at once");

        let mut rate = control();
        let behind = |_| vec![sending(secs_of(2.0, 6160), 0, 0.0)];
        let now = looks(&mut rate, Instant::now(), 2.5, behind);
        assert_eq!(rate.target(), 4200, "held down by the upload");
        rate.set_asked(8000, LIMIT);
        assert_eq!(rate.target(), 4200, "raised: still held down");
        let room = |_| vec![sending(0, 0, 9000.0)];
        // The room is judged over a window, then held six seconds.
        let now = looks(&mut rate, now, 10.0, room);
        assert_eq!(rate.target(), 5000, "climbing in steps of the new rate's tenth");
        looks(&mut rate, now, 120.0, room);
        assert_eq!(rate.target(), 8000, "to the new rate");
        rate.set_asked(2000, LIMIT);
        assert_eq!(rate.target(), 2000, "lowered under it: down at once");

        rate.set_asked(250, LIMIT);
        assert_eq!((rate.floor(), rate.target()), (250, 250), "its own floor");
        rate.set_asked(6000, LIMIT);
        assert_eq!((rate.floor(), rate.target()), (415, 6000), "the picture's floor again");
    }

    /// The operator's Wi-Fi switched to another network: the connection
    /// made over the old one died, its stream piled up and the rate was cut
    /// for it. Reconnected over the new network, the rate goes back to what
    /// the upload carried before rather than climbing from the floor — and
    /// a reconnect over the same network leaves it where it was.
    #[test]
    fn a_reconnect_over_a_new_network_takes_back_the_rate_cut_for_the_old() {
        let mut rate = control();
        let now = looks(&mut rate, Instant::now(), 3.0, |_| vec![sending(0, 0, 6100.0)]);
        assert_eq!(rate.target(), 6000);
        // The old way dies: what waits grows on, slowly enough that between
        // cuts it is held — no rate the upload carried.
        let dying = |t: f64| vec![sending(secs_of(1.0 + t * 0.1, 6160), 0, 0.0)];
        let now = looks(&mut rate, now, 30.0, dying);
        let cut = rate.target();
        assert!(cut < 3000, "cut for the dead connection: {cut}");
        let again = Seen {
            generation: 2,
            live_secs: 0.5,
            ..sending(0, 0, 0.0)
        };
        let now = looks(&mut rate, now, 1.0, |_| vec![again]);
        assert_eq!(rate.target(), cut, "the same network: left as it was");
        let new_way = Seen {
            generation: 3,
            network: 1,
            live_secs: 0.5,
            ..sending(0, 0, 0.0)
        };
        looks(&mut rate, now, 1.0, |_| vec![new_way]);
        assert_eq!(rate.target(), 6000, "back to what the upload carried before");
    }

    /// Two destinations: one's way dies and is cut for, then it reconnects
    /// while the other has room all along. The rate the other carries then
    /// is no rate the first's way carried: reconnected over the new
    /// network, the first takes back the rate from before its trouble.
    #[test]
    fn a_destination_reconnecting_keeps_the_rate_to_take_back() {
        let mut rate = control();
        let healthy = || sending(0, 0, 9000.0);
        let now = looks(&mut rate, Instant::now(), 3.0, |_| vec![sending(0, 0, 6100.0), healthy()]);
        assert_eq!(rate.target(), 6000);
        let dying = |t: f64| vec![sending(secs_of(1.0 + t * 0.1, 6160), 0, 0.0), healthy()];
        let now = looks(&mut rate, now, 30.0, dying);
        let cut = rate.target();
        assert!(cut < 3000, "cut for the dead connection: {cut}");
        let reconnecting = Seen {
            sending: false,
            ..sending(0, 0, 0.0)
        };
        let now = looks(&mut rate, now, 10.0, |_| vec![reconnecting, healthy()]);
        let new_way = Seen {
            generation: 2,
            network: 1,
            live_secs: 0.5,
            ..sending(0, 0, 0.0)
        };
        looks(&mut rate, now, 1.0, |_| vec![new_way, healthy()]);
        assert_eq!(rate.target(), 6000, "back to the rate before its trouble");
    }

    /// A destination switched on mid-stream connects for the first time:
    /// no new way out, whatever the count of network changes says — the
    /// rate another's congestion holds down stays down.
    #[test]
    fn a_destination_switched_on_is_no_new_network() {
        let mut rate = control();
        let off = Seen {
            sending: false,
            generation: 0,
            network: 0,
            live_secs: 0.0,
            ..sending(0, 0, 0.0)
        };
        let behind = |_| vec![sending(secs_of(2.0, 6160), 0, 0.0), off];
        let now = looks(&mut rate, Instant::now(), 2.5, behind);
        assert_eq!(rate.target(), 4200, "held down by the first");
        let switched_on = Seen {
            generation: 7,
            network: 3,
            live_secs: 0.5,
            ..sending(0, 0, 0.0)
        };
        let held = |_| vec![sending(secs_of(1.2, 4360), 0, 0.0), switched_on];
        looks(&mut rate, now, 0.5, held);
        assert_eq!(rate.target(), 4200, "not taken back for a first connection");
    }

    /// Two destinations, one behind, the other keeping up with a still
    /// slide held Variable — sending a few hundred kilobits because that is
    /// all the picture needs. The cut goes by what the one behind gets out,
    /// not by the one whose upload has room.
    #[test]
    fn a_destination_keeping_up_says_nothing_of_its_upload() {
        let mut rate = control();
        let seen = |_| {
            vec![
                sending(secs_of(2.0, 6160), 0, 1500.0),
                sending(0, 0, 300.0),
            ]
        };
        looks(&mut rate, Instant::now(), 2.5, seen);
        let target = rate.target();
        assert_eq!(target, ((1500.0 - 160.0) * HEADROOM) as u32, "by the one behind: {target}");
    }

    /// The box switched on mid-stream, a destination's first look already
    /// near overflow while it sends a slide's few hundred kilobits: cut at
    /// once, but by the usual share — not to what one look of it sent, with
    /// no window's figures to say its upload carries no more.
    #[test]
    fn one_look_says_nothing_of_an_upload() {
        let mut rate = control();
        rate.look(Instant::now(), &[sending(LIMIT / 2, 0, 300.0)]);
        assert_eq!(rate.target(), 4200);
    }

    /// A rate asked for under the floor is its own floor.
    #[test]
    fn a_low_rate_is_its_own_floor() {
        let mut rate = RateControl::new(250, 96, 300, LIMIT);
        let behind = |_| vec![sending(secs_of(2.0, 346), 0, 0.0)];
        looks(&mut rate, Instant::now(), 3.0, behind);
        assert_eq!(rate.target(), 250);
        assert!(rate.at_floor());
    }
}
