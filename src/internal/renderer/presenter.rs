//! Each output window's frames go on its screen from a thread of the
//! window's own (see [`Presenter`]).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use super::context::{GpuContext, PresentJob};

/// Frames waiting for a window's presenter at most: one more lets the oldest
/// go.
pub const WAITING_MOST: usize = 3;

/// Frames in a row a presenter takes with another already waiting behind —
/// a frame or more behind its screen — before it lets one go to catch up:
/// two seconds at 60 fps. A screen that misses a refresh is caught up with at
/// once (see [`CatchUp`]); this is for a delay with no missed refresh behind
/// it.
const BEHIND_TAKES: u32 = 120;

/// A wait for a frame to draw into this many refreshes long means the screen
/// missed one: its frames came back a refresh late.
const MISSED_REFRESH: f64 = 1.5;

/// A wait for a frame to draw into this much of a refresh or more means none
/// was free until the screen took the next one up: the window's frames are
/// queued a whole frame deep at its screen.
const QUEUED_WAIT: f64 = 0.85;

/// Presents after such a wait (less [`QUEUED_FORGIVE`] for each not) before
/// a frame is let go, to take that frame of delay away: two seconds at 60
/// fps. A second output — its
/// screen on a clock apart from the show's — sat a frame deep for good once
/// its frames had queued up, a frame later than the first (32.6 ms from the
/// engine to its glass against 20.6, a laptop, 2026-10-03): with every frame
/// shown in turn, nothing ever drained it.
const QUEUED_TAKES: u32 = 120;

/// A present that waited less than [`QUEUED_WAIT`] takes this many off the
/// count of queued presents rather than starting it again. A screen queued a
/// frame deep still lets a present through sooner now and then — 9–11 ms
/// every few seconds against 16 ms the rest (an LED wall, a laptop,
/// 2026-10-04) — and a count started again at each never reached
/// [`QUEUED_TAKES`]: the wall stayed a frame deep, its picture 33 ms later,
/// for good. Frames that come through in time more often than one in five
/// keep the count at nothing.
const QUEUED_FORGIVE: u32 = 4;

/// Presents after a frame let go for the queue before another may be, should
/// that one not have emptied it: ten seconds at 60 fps. Never again, as it
/// once was, left a queue that the one frame did not empty a frame deep for
/// good.
const QUEUED_AGAIN: u32 = 600;

/// Puts an output window's frames on its screen, on a thread of its own.
///
/// A present waits for the window to free a frame to draw into: ~11 ms of
/// every frame with frames held on screen, and up to a second while macOS
/// holds on to the window's frames — at every app switch with Stage Manager
/// on (a laptop, 2026-10-02). Waited for on the engine thread, that second
/// froze every output with it, and the control window, recording and stream.
/// The engine thread hands each frame to the window's presenter instead and
/// goes on.
///
/// Frames wait in the order drawn, each with a picture of its own (see
/// [`PresentJob`]), and go on the screen one after another. A single slot
/// once held the newest frame, and the picture copied was the one the engine
/// draws each frame over: with a screen being captured, whose frames macOS
/// gave back late and unevenly, 60% of an LED wall's frames went up with the
/// next frame's picture and 75 in 20 s were replaced unseen (a laptop,
/// 2026-10-02). Now a frame is let go only when more than [`WAITING_MOST`]
/// wait — the oldest, as a screen held up that long has missed it anyway —
/// or to catch up (see [`CatchUp`]).
pub struct Presenter {
    frames: Queue<PresentJob>,
}

impl Presenter {
    /// Start presenting the frames of the output window `name`.
    pub fn start(name: &str, gpu: GpuContext) -> Self {
        let mut catch_up = CatchUp::default();
        Self {
            frames: Queue::start(
                &format!("nida-present-{name}"),
                WAITING_MOST,
                move |job: PresentJob, waiting: usize| {
                    if catch_up.present(waiting) {
                        let refresh = job.refresh();
                        let held = job.held();
                        // A frame that could not be presented — the window
                        // closing, its screen gone or changing — says nothing
                        // of how the screen keeps up.
                        if let Some(waited) = job.present(&gpu) {
                            catch_up.presented(waited, refresh, held);
                        }
                    } else {
                        job.let_go_to_catch_up();
                    }
                },
            ),
        }
    }

    /// Hand over a frame, to be presented after those waiting. How many of
    /// those were let go to make room for it (0 or 1).
    pub fn hand(&self, job: PresentJob) -> usize {
        self.frames.hand(job)
    }

    /// Stop presenting: frames waiting are let go; one being presented is
    /// finished.
    pub fn stop(&self) {
        self.frames.stop();
    }
}

/// When a presenter lets a frame go to catch up with its screen.
///
/// A screen that misses a refresh shows the frame before twice, and every
/// frame after it a refresh late from then on — unless one is let go. Let go
/// at once, the frame after the missed refresh, the eye sees one jump where
/// the screen stalled; let go a second later (as this first did — 9 missed
/// refreshes and 8 frames let go in 20 s on an LED wall, a laptop with a
/// screen captured, 2026-10-02), it saw two. A delay with no missed refresh
/// behind it — a frame or more waiting behind each one taken — is caught up
/// with after [`BEHIND_TAKES`] in a row.
#[derive(Debug, Default)]
struct CatchUp {
    /// Frames taken in a row with another waiting behind.
    behind: u32,
    /// The frame presented last went up a refresh late.
    missed: bool,
    /// Presents with the window's frames queued a frame deep at its screen
    /// (see [`QUEUED_WAIT`]), less [`QUEUED_FORGIVE`] for each not.
    queued: u32,
    /// Presents to go before another frame may be let go for the queue (see
    /// [`QUEUED_AGAIN`]).
    queued_rest: u32,
}

impl CatchUp {
    /// Whether to present the frame just taken, `waiting` more behind it:
    /// not the one after a missed refresh, nor the one that ends
    /// [`QUEUED_TAKES`] queued a frame deep, nor the one that makes
    /// [`BEHIND_TAKES`] in a row with one waiting. A frame let go for any of
    /// them counts for all: one let go is a frame caught up with, and two
    /// in a row — a missed refresh at the end of a deep queue — would be one
    /// too many.
    fn present(&mut self, waiting: usize) -> bool {
        let catch_up = if self.missed {
            true
        } else if self.queued >= QUEUED_TAKES && self.queued_rest == 0 {
            self.queued_rest = QUEUED_AGAIN;
            true
        } else {
            waiting > 0 && self.behind + 1 >= BEHIND_TAKES
        };
        if catch_up {
            self.missed = false;
            self.queued = 0;
            self.behind = 0;
            return false;
        }
        self.behind = if waiting == 0 { 0 } else { self.behind + 1 };
        true
    }

    /// The frame just presented waited `waited` for a frame to draw into, on
    /// a screen showing a frame every `refresh`: a refresh and a half or more
    /// means the screen missed one; most of a refresh, frames queued a frame
    /// deep — except where each is `held` on screen a frame's time, which
    /// keeps them queued by design.
    fn presented(
        &mut self,
        waited: std::time::Duration,
        refresh: std::time::Duration,
        held: bool,
    ) {
        let refreshes = waited.as_secs_f64() / refresh.as_secs_f64().max(1e-6);
        self.missed = refreshes >= MISSED_REFRESH;
        self.queued_rest = self.queued_rest.saturating_sub(1);
        self.queued = if held {
            0
        } else if refreshes < QUEUED_WAIT {
            self.queued.saturating_sub(QUEUED_FORGIVE)
        } else {
            self.queued + 1
        };
    }
}

/// Things for a thread of its own to take, oldest first: at most `most`
/// wait, one more lets the oldest go, and handing over never waits.
struct Queue<T> {
    shared: Arc<Shared<T>>,
}

struct Shared<T> {
    waiting: Mutex<Waiting<T>>,
    ready: Condvar,
    most: usize,
}

struct Waiting<T> {
    items: VecDeque<T>,
    stopped: bool,
}

impl<T: Send + 'static> Queue<T> {
    /// Start a thread named `name` that gives each thing handed over to
    /// `take`, in turn, with how many still wait behind it.
    fn start(name: &str, most: usize, mut take: impl FnMut(T, usize) + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            waiting: Mutex::new(Waiting {
                items: VecDeque::with_capacity(most + 1),
                stopped: false,
            }),
            ready: Condvar::new(),
            most: most.max(1),
        });
        let theirs = Arc::clone(&shared);
        let what = format!("Thread {name}");
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                raise_priority(&what);
                while let Some((item, waiting)) = theirs.next() {
                    take(item, waiting);
                }
            });
        if let Err(e) = spawned {
            log::error!("No thread for {name}: {e}");
        }
        Self { shared }
    }
}

impl<T> Queue<T> {
    /// Hand over the next thing to take, after those waiting. How many of
    /// those were let go to make room (0 or 1).
    fn hand(&self, item: T) -> usize {
        let let_go = {
            let mut waiting = self.shared.lock();
            let mut let_go = Vec::new();
            while waiting.items.len() >= self.shared.most {
                let_go.extend(waiting.items.pop_front());
            }
            waiting.items.push_back(item);
            let_go
        };
        self.shared.ready.notify_one();
        // Let go here, not holding the lock: a frame lets its picture go.
        let_go.len()
    }

    /// Stop the thread once it has done with what it took; things waiting
    /// are let go now.
    fn stop(&self) {
        let waiting = {
            let mut waiting = self.shared.lock();
            waiting.stopped = true;
            std::mem::take(&mut waiting.items)
        };
        drop(waiting);
        self.shared.ready.notify_one();
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        self.stop();
    }
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, Waiting<T>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The oldest thing waiting and how many wait behind it, waiting for
    /// one; `None` once stopped.
    fn next(&self) -> Option<(T, usize)> {
        let mut waiting = self.lock();
        loop {
            if waiting.stopped {
                return None;
            }
            if let Some(item) = waiting.items.pop_front() {
                let behind = waiting.items.len();
                return Some((item, behind));
            }
            waiting = self
                .ready
                .wait(waiting)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Run the calling thread at the priority macOS gives work the user is
/// watching: frames on their way to a screen are exactly that. `what` names
/// the thread should it not be raised.
pub(crate) fn raise_priority(what: &str) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: sets the calling thread's own class; no pointers.
        let set = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        if set != 0 {
            log::warn!("{what} priority not raised: {set}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = what;
}

/// Run the calling thread on the Mach real-time (time-constraint) policy:
/// every `period` it may need up to `computation` of a CPU, had within
/// `constraint` of being woken — the kernel takes at least half of
/// `constraint` as `computation`, and time spent waiting counts for none.
/// The scheduler then runs it on time whichever app is in front, as Core
/// Audio's threads, where the interactive priority of [`raise_priority`]
/// loses its boost once another app is (measured: a Blackmagic output's
/// frames, 2026-10-05). Where it is refused, and off macOS, that priority
/// instead. Whether it took.
pub(crate) fn run_in_real_time(
    what: &str,
    period: std::time::Duration,
    computation: std::time::Duration,
    constraint: std::time::Duration,
) -> bool {
    #[cfg(target_os = "macos")]
    {
        match real_time::set(period, computation, constraint) {
            Ok(()) => {
                let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
                // As the kernel took it, not as asked.
                let (period, computation, constraint) =
                    real_time::get().unwrap_or((period, computation, constraint));
                log::info!(
                    "{what} runs in real time: {:.1} ms of a CPU every {:.1} ms, within {:.1} ms",
                    ms(computation),
                    ms(period),
                    ms(constraint)
                );
                true
            }
            Err(code) => {
                log::warn!("{what} not in real time (kernel said {code}): at interactive priority");
                raise_priority(what);
                false
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (period, computation, constraint);
        raise_priority(what);
        false
    }
}

/// The thread `of`'s period, computation and constraint, if it runs in real
/// time (see [`run_in_real_time`]).
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn real_time_of<T>(
    of: &std::thread::JoinHandle<T>,
) -> Option<(std::time::Duration, std::time::Duration, std::time::Duration)> {
    use std::os::unix::thread::JoinHandleExt;
    // SAFETY: a thread still joinable: its port, not a new right to give back.
    #[allow(deprecated)]
    let thread = unsafe { libc::pthread_mach_thread_np(of.as_pthread_t()) };
    real_time::get_of(thread)
}

/// The calling thread's Mach time-constraint policy (see [`run_in_real_time`]).
// libc marks its mach calls deprecated in favour of the mach2 crate, which
// Nida does not use (as in `host_clock.rs`).
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod real_time {
    use std::time::Duration;

    extern "C" {
        fn mach_port_deallocate(task: libc::mach_port_t, name: libc::mach_port_t) -> libc::c_int;
    }

    /// Nanoseconds to Mach ticks, and back.
    fn timebase() -> Option<(u64, u64)> {
        let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: fills the struct given.
        let ok = unsafe { libc::mach_timebase_info(&raw mut timebase) } == 0;
        (ok && timebase.numer != 0 && timebase.denom != 0)
            .then(|| (u64::from(timebase.numer), u64::from(timebase.denom)))
    }

    /// Run `with` on the calling thread's port, given back after.
    fn on_this_thread<T>(with: impl FnOnce(libc::thread_t) -> T) -> T {
        // SAFETY: the calling thread's own port, a right of ours given back.
        unsafe {
            let thread = libc::mach_thread_self();
            let result = with(thread);
            mach_port_deallocate(libc::mach_task_self(), thread);
            result
        }
    }

    pub(super) fn set(period: Duration, computation: Duration, constraint: Duration) -> Result<(), i32> {
        let (numer, denom) = timebase().ok_or(-1)?;
        let ticks = |d: Duration| {
            let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
            u32::try_from(nanos.saturating_mul(denom) / numer).unwrap_or(u32::MAX)
        };
        let mut policy = libc::thread_time_constraint_policy {
            period: ticks(period),
            computation: ticks(computation),
            constraint: ticks(constraint),
            preemptible: 1,
        };
        let set = on_this_thread(|thread| {
            // SAFETY: the policy struct is the size its count says.
            unsafe {
                libc::thread_policy_set(
                    thread,
                    libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                    (&raw mut policy).cast::<libc::integer_t>(),
                    libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
                )
            }
        });
        if set == 0 {
            Ok(())
        } else {
            Err(set)
        }
    }

    /// The calling thread's period, computation and constraint, if it runs
    /// in real time.
    pub(super) fn get() -> Option<(Duration, Duration, Duration)> {
        on_this_thread(get_of)
    }

    /// The thread `thread`'s, so.
    pub(super) fn get_of(thread: libc::thread_t) -> Option<(Duration, Duration, Duration)> {
        let (numer, denom) = timebase()?;
        let mut policy = libc::thread_time_constraint_policy {
            period: 0,
            computation: 0,
            constraint: 0,
            preemptible: 0,
        };
        let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
        let mut default: libc::boolean_t = 0;
        // SAFETY: the policy struct is the size its count says.
        let got = unsafe {
            libc::thread_policy_get(
                thread,
                libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
                (&raw mut policy).cast::<libc::integer_t>(),
                &raw mut count,
                &raw mut default,
            )
        };
        let nanos = |ticks: u32| Duration::from_nanos(u64::from(ticks) * numer / denom);
        (got == 0 && default == 0).then(|| {
            (
                nanos(policy.period),
                nanos(policy.computation),
                nanos(policy.constraint),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CatchUp, Queue, BEHIND_TAKES, QUEUED_AGAIN, QUEUED_TAKES};
    const REFRESH: Duration = Duration::from_micros(16_667);
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    /// A thread asking to run in real time on a Mac is let to, on the terms
    /// asked — the kernel raising its CPU time to half its deadline — and
    /// runs on as any thread; one not asking is not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_thread_can_run_in_real_time() {
        let (took, policy) = std::thread::spawn(|| {
            let took = super::run_in_real_time(
                "A test thread",
                Duration::from_micros(16_667),
                Duration::from_millis(2),
                Duration::from_millis(6),
            );
            std::thread::sleep(Duration::from_millis(5));
            (took, super::real_time::get())
        })
        .join()
        .unwrap();
        assert!(took);
        let (period, computation, constraint) = policy.expect("in real time");
        let near = |a: Duration, b: Duration| a.abs_diff(b) < Duration::from_micros(5);
        assert!(near(period, Duration::from_micros(16_667)), "{period:?}");
        assert!(near(constraint, Duration::from_millis(6)), "{constraint:?}");
        assert!(near(computation, Duration::from_millis(3)), "{computation:?}");
        let other = std::thread::spawn(super::real_time::get).join().unwrap();
        assert_eq!(other, None, "only the thread that asked");
    }

    /// A window's screen holding its presenter up never holds up the engine,
    /// and costs it no frame within what may wait: handing over is
    /// immediate, and the presenter then takes every frame in the order
    /// drawn, told how many wait behind each.
    #[test]
    fn a_held_up_presenter_takes_every_frame_in_turn_and_never_holds_up_the_hand_over() {
        let (busy_tx, busy_rx) = mpsc::channel::<()>();
        let (took_tx, took_rx) = mpsc::channel();
        let queue = Queue::start("test-present", 3, move |frame: u32, waiting: usize| {
            took_tx.send((frame, waiting)).expect("the test is listening");
            if frame == 1 {
                // Held up, as by a screen not freeing a frame.
                let _ = busy_rx.recv_timeout(Duration::from_secs(5));
            }
        });
        assert_eq!(queue.hand(1), 0);
        assert_eq!(took_rx.recv_timeout(Duration::from_secs(5)), Ok((1, 0)));

        let started = Instant::now();
        for frame in 2..=4 {
            assert_eq!(queue.hand(frame), 0, "frame {frame} lets none go");
        }
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "{:?}",
            started.elapsed()
        );

        busy_tx.send(()).expect("the presenter is held up");
        let took: Vec<_> = (0..3)
            .map(|_| took_rx.recv_timeout(Duration::from_secs(5)).expect("taken"))
            .collect();
        assert_eq!(took, [(2, 2), (3, 1), (4, 0)], "in turn, none lost");
    }

    /// More than may wait lets the oldest go, counted, and the newest are
    /// still taken in turn.
    #[test]
    fn more_frames_than_may_wait_let_the_oldest_go() {
        let (busy_tx, busy_rx) = mpsc::channel::<()>();
        let (took_tx, took_rx) = mpsc::channel();
        let queue = Queue::start("test-present", 2, move |frame: u32, _: usize| {
            took_tx.send(frame).expect("the test is listening");
            if frame == 1 {
                let _ = busy_rx.recv_timeout(Duration::from_secs(5));
            }
        });
        queue.hand(1);
        assert_eq!(took_rx.recv_timeout(Duration::from_secs(5)), Ok(1));
        assert_eq!(queue.hand(2), 0);
        assert_eq!(queue.hand(3), 0);
        assert_eq!(queue.hand(4), 1, "2 let go");
        assert_eq!(queue.hand(5), 1, "3 let go");
        busy_tx.send(()).expect("held up");
        assert_eq!(took_rx.recv_timeout(Duration::from_secs(5)), Ok(4));
        assert_eq!(took_rx.recv_timeout(Duration::from_secs(5)), Ok(5));
        assert_eq!(
            took_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    /// A window whose frames have queued a whole frame deep at its screen —
    /// each waiting most of a refresh for a frame to draw into — lets one go
    /// after a while, once, to take that frame of delay away; a window that
    /// waits less, or holds its frames on screen on purpose, never does.
    #[test]
    fn a_window_queued_a_frame_deep_lets_one_frame_go_once() {
        let deep = Duration::from_micros(16_400);
        let mut catch_up = CatchUp::default();
        for _ in 0..QUEUED_TAKES {
            assert!(catch_up.present(0));
            catch_up.presented(deep, REFRESH, false);
        }
        assert!(!catch_up.present(0), "a frame of delay let go");
        assert!(catch_up.present(0), "once");
        catch_up.presented(Duration::from_millis(1), REFRESH, false);
        for _ in 0..QUEUED_TAKES * 3 {
            assert!(catch_up.present(0), "not queued any more");
            catch_up.presented(Duration::from_micros(5_200), REFRESH, false);
        }
        let mut held = CatchUp::default();
        for _ in 0..QUEUED_TAKES * 3 {
            assert!(held.present(0), "held on purpose");
            held.presented(deep, REFRESH, true);
        }
    }

    /// A window queued a frame deep whose presents now and then go through
    /// sooner — one in twenty at 10 ms against 16.3 the rest, as an LED wall's
    /// did for 45 s (a laptop, 2026-10-04) — still lets a frame go within
    /// seconds; and, the queue still deep after, another ten seconds on, not
    /// sooner.
    #[test]
    fn a_window_queued_a_frame_deep_but_now_and_then_through_sooner_catches_up() {
        let (deep, sooner) = (Duration::from_micros(16_300), Duration::from_millis(10));
        let mut catch_up = CatchUp::default();
        let mut let_go = Vec::new();
        for n in 0..QUEUED_AGAIN * 2 {
            if catch_up.present(0) {
                catch_up.presented(if n % 20 == 19 { sooner } else { deep }, REFRESH, false);
            } else {
                let_go.push(n);
            }
        }
        assert!(let_go.first().is_some_and(|&n| n < QUEUED_TAKES * 2), "{let_go:?}");
        let again: Vec<u32> = let_go.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(!again.is_empty() && again.iter().all(|&gap| gap >= QUEUED_AGAIN), "{let_go:?}");
    }

    /// A missed refresh at the end of a deep queue lets one frame go, not
    /// two in a row.
    #[test]
    fn a_missed_refresh_at_the_end_of_a_deep_queue_lets_one_frame_go() {
        let mut catch_up = CatchUp::default();
        for _ in 0..QUEUED_TAKES - 1 {
            assert!(catch_up.present(0));
            catch_up.presented(Duration::from_micros(16_400), REFRESH, false);
        }
        assert!(catch_up.present(0));
        catch_up.presented(Duration::from_millis(28), REFRESH, false);
        assert!(!catch_up.present(0), "let go after the missed refresh");
        assert!(catch_up.present(0), "and not the next one too");
    }

    /// A frame let go for a deep queue that stays deep is not followed by
    /// another every two seconds — only ten seconds on, should the queue be
    /// deep still: that one frame did not empty it.
    #[test]
    fn a_frame_let_go_for_a_queue_that_stays_deep_is_let_go_again_only_ten_seconds_on() {
        let deep = Duration::from_micros(16_400);
        let mut catch_up = CatchUp::default();
        let mut let_go = Vec::new();
        for n in 0..QUEUED_AGAIN * 2 + QUEUED_TAKES {
            if catch_up.present(0) {
                catch_up.presented(deep, REFRESH, false);
            } else {
                let_go.push(n);
            }
        }
        assert_eq!(let_go, [QUEUED_TAKES, QUEUED_TAKES + QUEUED_AGAIN + 1]);
    }

    /// The frame after a missed refresh is let go, so the screen is back in
    /// step at once; a wait of a refresh or a little more is no miss.
    #[test]
    fn the_frame_after_a_missed_refresh_is_let_go() {
        let mut catch_up = CatchUp::default();
        for waited in [16.5, 20.5, 24.0, 10.0] {
            assert!(catch_up.present(0));
            catch_up.presented(Duration::from_secs_f64(waited / 1000.0), REFRESH, false);
        }
        assert!(catch_up.present(0), "no refresh missed");
        catch_up.presented(Duration::from_millis(28), REFRESH, false);
        assert!(!catch_up.present(0), "after the missed refresh: let go");
        assert!(catch_up.present(0), "only the one");
        catch_up.presented(Duration::from_millis(16), REFRESH, false);
        assert!(catch_up.present(1));
    }

    /// A presenter a frame behind with no refresh missed lets one frame go
    /// after a while, once, and one keeping up never does.
    #[test]
    fn a_presenter_behind_for_a_while_lets_one_frame_go() {
        let mut catch_up = CatchUp::default();
        for _ in 0..1000 {
            assert!(catch_up.present(0), "keeping up");
        }
        for take in 1..BEHIND_TAKES {
            assert!(catch_up.present(1), "take {take} behind");
        }
        assert!(!catch_up.present(1), "a second behind: one let go");
        assert!(catch_up.present(1), "and counting starts again");
        assert!(catch_up.present(0));
        for _ in 1..BEHIND_TAKES {
            assert!(catch_up.present(2));
        }
        assert!(!catch_up.present(2));
    }

    /// Stopping lets the frames waiting go at once — each holds a picture —
    /// and the thread ends after the frame it has.
    #[test]
    fn stopping_lets_waiting_frames_go_and_ends_the_thread() {
        struct Frame(Arc<AtomicBool>);
        impl Drop for Frame {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let (busy_tx, busy_rx) = mpsc::channel::<()>();
        let (took_tx, took_rx) = mpsc::channel::<()>();
        let (ended_tx, ended_rx) = mpsc::channel::<()>();
        let queue = Queue::start("test-present", 3, move |_frame: Frame, _: usize| {
            took_tx.send(()).expect("the test is listening");
            let _ = busy_rx.recv_timeout(Duration::from_secs(5));
            let _ = ended_tx.send(());
        });
        let first = Arc::new(AtomicBool::new(false));
        let waiting = [Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false))];
        queue.hand(Frame(Arc::clone(&first)));
        took_rx.recv_timeout(Duration::from_secs(5)).expect("taken");
        for w in &waiting {
            queue.hand(Frame(Arc::clone(w)));
        }

        queue.stop();
        assert!(
            waiting.iter().all(|w| w.load(Ordering::SeqCst)),
            "let go at once"
        );
        assert!(!first.load(Ordering::SeqCst), "still being presented");
        busy_tx.send(()).expect("the presenter is busy");
        ended_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("finished");
        drop(queue);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !first.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            first.load(Ordering::SeqCst),
            "the thread ended, done with it"
        );
    }
}
