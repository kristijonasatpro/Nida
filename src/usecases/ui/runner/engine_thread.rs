//! The engine on a thread of its own, so the outputs keep time whatever the
//! control window is doing.
//!
//! Measured in Nida (laptop, a picture-in-picture scene at 60 fps): with everything on the
//! main thread, that thread spent about 11 of every 16.7 ms waiting for the
//! control window's next frame to draw into, and any moment of UI work over
//! the few milliseconds left cost the TV a frame. The standalone probe
//! (`probes/metal-link-probe`) showed the cure: the main thread frozen 25 ms
//! every second made 13–16 hitches a run on screen with rendering on the
//! main thread, and 0–1 with rendering on a thread of its own.
//!
//! So the engine — commands, inputs, the mixer, every output, the state the
//! API reads — runs here, woken by the refresh of the screen the show goes
//! to (see `vsync`), or by a timer where there is none. The control window
//! (winit and egui) stays on the main thread and draws at its own pace.
//!
//! The engine is handed between the two threads like a baton ([`Baton`]):
//! this thread holds it for each frame; the main thread holds it for
//! moments — to read what the control window shows, to apply what the
//! operator did, to make windows — and never while it lays out the UI or
//! waits for a frame of its own to draw into. Nothing holding the baton
//! waits for the other thread: the engine never asks winit or the macOS
//! window system for anything (see `OsWindow`), which would wait for the
//! main thread.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::app::VardaApp;

use super::vsync;

/// The engine, held by one thread at a time.
pub(super) struct Baton {
    slot: Mutex<Slot>,
    turn: Condvar,
}

#[derive(Default)]
struct Slot {
    app: Option<Box<VardaApp>>,
    /// Taken for good (see [`Baton::close`]): the engine thread stops.
    closed: bool,
    /// The engine thread has ended — with the engine, if it panicked
    /// holding it.
    gone: bool,
    /// Threads waiting in [`Baton::take`].
    waiting: u32,
    /// Times the engine has been taken.
    takes: u64,
}

impl Baton {
    pub(super) fn new(app: Box<VardaApp>) -> Self {
        Self {
            slot: Mutex::new(Slot {
                app: Some(app),
                ..Slot::default()
            }),
            turn: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The engine, once the other thread has put it down: `None` once the
    /// baton is closed, or the engine thread is gone.
    pub(super) fn take(&self) -> Option<Box<VardaApp>> {
        match self.take_within(None) {
            Taken::Engine(app) => Some(app),
            Taken::Late | Taken::Closed => None,
        }
    }

    /// The engine, waiting at most `patience` (`None`: as long as it takes)
    /// for the other thread to put it down.
    pub(super) fn take_within(&self, patience: Option<Duration>) -> Taken {
        let until = patience.map(|p| Instant::now() + p);
        let mut slot = self.lock();
        loop {
            if slot.closed || slot.gone {
                return Taken::Closed;
            }
            if let Some(app) = slot.app.take() {
                slot.takes += 1;
                // A thread letting this one have its turn goes on at once.
                self.turn.notify_all();
                return Taken::Engine(app);
            }
            slot.waiting += 1;
            slot = match until {
                None => self.turn.wait(slot).unwrap_or_else(PoisonError::into_inner),
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        slot.waiting -= 1;
                        return Taken::Late;
                    }
                    self.turn
                        .wait_timeout(slot, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
            slot.waiting -= 1;
        }
    }

    pub(super) fn put(&self, app: Box<VardaApp>) {
        self.lock().app = Some(app);
        self.turn.notify_all();
    }

    /// Put the engine down, and when a thread is waiting for it, let that
    /// thread have it before coming back for it — at most `patience` — so a
    /// thread that takes it again at once (an uncapped frame rate) cannot
    /// keep it from the other.
    fn put_and_yield(&self, app: Box<VardaApp>, patience: Duration) {
        let mut slot = self.lock();
        slot.app = Some(app);
        self.turn.notify_all();
        if slot.waiting == 0 {
            return;
        }
        let takes = slot.takes;
        let until = Instant::now() + patience;
        while slot.takes == takes && !slot.closed {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            slot = self
                .turn
                .wait_timeout(slot, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Take the engine for good once the engine thread has put it down —
    /// waiting at most `patience` — that thread's next [`Self::take`]
    /// returns `None`, and it stops. `None` if the engine thread went with
    /// the engine (a panic), or did not put it down in time.
    pub(super) fn close(&self, patience: Duration) -> Option<Box<VardaApp>> {
        let until = Instant::now() + patience;
        let mut slot = self.lock();
        slot.closed = true;
        self.turn.notify_all();
        loop {
            if let Some(app) = slot.app.take() {
                return Some(app);
            }
            let left = until.saturating_duration_since(Instant::now());
            if slot.gone || left.is_zero() {
                return None;
            }
            slot = self
                .turn
                .wait_timeout(slot, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The engine thread has ended.
    fn gone(&self) {
        self.lock().gone = true;
        self.turn.notify_all();
    }

    /// Whether the engine thread has ended.
    pub(super) fn is_gone(&self) -> bool {
        self.lock().gone
    }
}

/// What [`Baton::take_within`] came to.
pub(super) enum Taken {
    Engine(Box<VardaApp>),
    /// Still held by the other thread when patience ran out.
    Late,
    /// Closed, or the engine thread gone.
    Closed,
}

/// Where the engine thread is in its loop, for a report when it keeps the
/// engine too long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Phase {
    Waiting,
    Prepare,
    GpuWait,
    Mixer,
    Outputs,
    Publish,
    Done,
}

impl Phase {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Prepare,
            2 => Self::GpuWait,
            3 => Self::Mixer,
            4 => Self::Outputs,
            5 => Self::Publish,
            6 => Self::Done,
            _ => Self::Waiting,
        }
    }
}

/// Wakes the engine thread: a refresh calling for a frame, or the thread
/// told to stop.
#[derive(Default)]
struct Signal {
    state: Mutex<SignalState>,
    changed: Condvar,
}

#[derive(Default)]
struct SignalState {
    /// Frames the display clock has asked for.
    ticks: u64,
    stop: bool,
}

/// What a wait for the display clock came to.
#[derive(Debug, PartialEq, Eq)]
enum Woken {
    /// The display clock asked for frames: this many since the last wait.
    Ticks(u64),
    TimedOut,
    Stop,
}

impl Signal {
    fn lock(&self) -> MutexGuard<'_, SignalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tick(&self) {
        self.lock().ticks += 1;
        self.changed.notify_all();
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.lock().stop
    }

    /// Wait for the display clock to ask for a frame after `seen` asks, at
    /// most `timeout`.
    fn wait_tick(&self, seen: &mut u64, timeout: Duration) -> Woken {
        let until = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.stop {
                return Woken::Stop;
            }
            if state.ticks != *seen {
                let new = state.ticks.wrapping_sub(*seen);
                *seen = state.ticks;
                return Woken::Ticks(new);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Woken::TimedOut;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Sleep until `deadline`; `false` if told to stop meanwhile.
    fn sleep_until(&self, deadline: Instant) -> bool {
        let mut state = self.lock();
        loop {
            if state.stop {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The engine thread, while it runs.
pub(super) struct EngineThread {
    baton: Arc<Baton>,
    signal: Arc<Signal>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Where the thread is in its loop (see [`Phase`]).
    phase: Arc<std::sync::atomic::AtomicU8>,
    started: Instant,
}

impl EngineThread {
    /// Run `app` on a thread of its own; `wake_ui` is called after each
    /// frame, for the control window to show it. `gpu` is asked how far the
    /// GPU has got (see `EngineLoop::wait_for_gpu`); each output window's
    /// frames go on its screen from a thread of the window's own
    /// (`renderer::presenter::Presenter`).
    pub(super) fn start(
        app: Box<VardaApp>,
        gpu: crate::renderer::context::GpuContext,
        wake_ui: impl Fn() + Send + 'static,
    ) -> Self {
        let baton = Arc::new(Baton::new(app));
        let signal = Arc::new(Signal::default());
        let phase = Arc::new(std::sync::atomic::AtomicU8::new(Phase::Waiting as u8));
        let engine = EngineLoop::new(
            Arc::clone(&signal),
            Arc::clone(&phase),
            gpu,
            Box::new(wake_ui),
        );
        let handle = {
            let baton = Arc::clone(&baton);
            std::thread::Builder::new()
                .name("nida-engine".into())
                .spawn(move || engine.run(&baton))
                .expect("start the engine thread")
        };
        log::info!("Engine running on a thread of its own");
        Self {
            baton,
            signal,
            handle: Some(handle),
            phase,
            started: Instant::now(),
        }
    }

    /// Whether the thread is still in its first seconds: loading the show and
    /// building its shaders, when frames run long.
    pub(super) fn starting(&self) -> bool {
        self.started.elapsed() < STARTUP
    }

    /// Where the engine thread is in its loop.
    pub(super) fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub(super) fn baton(&self) -> &Baton {
        &self.baton
    }

    /// Stop the thread, once its frame is done, and have the engine back —
    /// `None` if the thread went with it (a panic), or kept it past
    /// [`STOP_PATIENCE`] (then left to end on its own).
    pub(super) fn stop(mut self) -> Option<Box<VardaApp>> {
        self.signal.stop();
        let app = self.baton.close(STOP_PATIENCE);
        if app.is_none() && !self.baton.is_gone() {
            log::error!(
                "The engine thread did not stop within {STOP_PATIENCE:?} (it was at {:?})",
                self.phase()
            );
            // Not joined: it may be waiting for this, the main thread.
            self.handle.take();
            return None;
        }
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                log::error!("The engine thread panicked");
            }
        }
        app
    }
}

impl Drop for EngineThread {
    fn drop(&mut self) {
        // Stopped without being asked back (the app ending another way): the
        // thread must not go on drawing into windows that are going.
        if let Some(handle) = self.handle.take() {
            self.signal.stop();
            if self.baton.close(STOP_PATIENCE).is_some() || self.baton.is_gone() {
                let _ = handle.join();
            }
        }
    }
}

/// Marks the engine thread gone however it ends, so a thread waiting for
/// the engine does not wait forever.
struct Gone<'a>(&'a Baton);

impl Drop for Gone<'_> {
    fn drop(&mut self) {
        self.0.gone();
    }
}

/// What the engine thread saw of the display clock, for the performance
/// figures.
#[derive(Debug, Clone, Copy, Default)]
struct LinkCounts {
    /// Asks for a frame that woke the thread.
    events: u64,
    /// Frames drawn.
    frames: u64,
    /// Asks that came while the thread was still busy with a frame: merged,
    /// a refresh with no frame of its own.
    collapsed: u64,
}

/// The engine thread's first seconds, when a long frame is expected.
const STARTUP: Duration = Duration::from_secs(5);

/// How long the engine thread waits for the GPU to finish a frame, at most.
const GPU_PATIENCE: Duration = Duration::from_millis(100);
/// How often it looks, meanwhile.
const GPU_CHECK: Duration = Duration::from_micros(200);

/// How long quitting waits for the engine thread to finish its frame.
const STOP_PATIENCE: Duration = Duration::from_secs(2);

/// How long the engine thread waits for a refresh before looking again at
/// what starts frames.
const TICK_TIMEOUT: Duration = Duration::from_millis(50);
/// How long the engine thread lets a waiting main thread have the engine
/// before coming back for it.
const YIELD_PATIENCE: Duration = Duration::from_millis(4);

/// The engine thread's frame loop: what starts each frame, and the frame.
struct EngineLoop {
    signal: Arc<Signal>,
    phase: Arc<std::sync::atomic::AtomicU8>,
    /// Asked how far the GPU has got (see `wait_for_gpu`).
    gpu: crate::renderer::context::GpuContext,
    wake_ui: Box<dyn Fn() + Send>,
    /// The screen refresh frames are started on, when there is one to
    /// follow; else the timer.
    display_clock: Option<vsync::DisplayClock>,
    /// When the screen to follow was last looked for.
    clock_checked: Option<Instant>,
    /// What starts frames, as last told to the engine.
    clock_reported: Option<(Option<u32>, u32, u32)>,
    /// Display clock asks seen.
    ticks_seen: u64,
    /// When the thread last woke for a frame.
    woke_at: Option<Instant>,
    /// The refresh that woke it, taken as it woke (see `wait`).
    woken_by: Option<Instant>,
    counts: LinkCounts,
    /// The ideal start of the next frame on the timer (see
    /// [`next_cadence_anchor`]).
    cadence_anchor: Option<Instant>,
    /// Set when the GPU has finished each of the last frames (and all
    /// submitted before it), the latest last.
    gpu_done: VecDeque<Arc<std::sync::atomic::AtomicBool>>,
    /// Frames the GPU may still be drawing when the next starts, as the
    /// engine last said.
    frames_in_flight: u32,
    publish_counter: u32,
}

impl EngineLoop {
    fn new(
        signal: Arc<Signal>,
        phase: Arc<std::sync::atomic::AtomicU8>,
        gpu: crate::renderer::context::GpuContext,
        wake_ui: Box<dyn Fn() + Send>,
    ) -> Self {
        Self {
            signal,
            phase,
            gpu,
            wake_ui,
            display_clock: None,
            clock_checked: None,
            clock_reported: None,
            ticks_seen: 0,
            woke_at: None,
            woken_by: None,
            counts: LinkCounts::default(),
            cadence_anchor: None,
            gpu_done: VecDeque::new(),
            frames_in_flight: 1,
            publish_counter: 0,
        }
    }

    fn run(mut self, baton: &Baton) {
        let _gone = Gone(baton);
        crate::renderer::presenter::raise_priority("Engine thread");
        let mut target_fps = 60;
        loop {
            self.set_phase(Phase::Waiting);
            if !self.wait(target_fps) {
                break;
            }
            let gpu_wait = self.wait_for_gpu();
            let asked = Instant::now();
            let Some(mut app) = baton.take() else {
                break;
            };
            let waited = asked.elapsed();
            self.frame(&mut app, waited, gpu_wait);
            target_fps = app.target_fps();
            self.set_phase(Phase::Done);
            baton.put_and_yield(app, YIELD_PATIENCE);
            (self.wake_ui)();
        }
        log::info!("Engine thread stopped");
    }

    /// Wait for the GPU to finish the frame `frames_in_flight - 1` back —
    /// its work, and all submitted before it (the control window's too) —
    /// before the engine is taken, and without wgpu's device lock: waited
    /// for with `poll(Wait)`, that lock is held the whole wait, and the
    /// control window's present needs it.
    fn wait_for_gpu(&mut self) -> Duration {
        let started = Instant::now();
        let behind = self.frames_in_flight.saturating_sub(1) as usize;
        let mut wait_for = None;
        while self.gpu_done.len() > behind {
            wait_for = self.gpu_done.pop_front();
        }
        let Some(done) = wait_for else {
            return Duration::ZERO;
        };
        self.set_phase(Phase::GpuWait);
        let finished = || done.load(std::sync::atomic::Ordering::Acquire);
        while !finished() {
            // Runs the callbacks of work the GPU has finished: this frame's
            // among them, when it has.
            let _ = self.gpu.device.poll(wgpu::PollType::Poll);
            if finished() || started.elapsed() > GPU_PATIENCE {
                break;
            }
            std::thread::sleep(GPU_CHECK);
        }
        started.elapsed()
    }

    fn set_phase(&self, phase: Phase) {
        self.phase
            .store(phase as u8, std::sync::atomic::Ordering::Relaxed);
    }

    /// Wait for the next frame to be due: the display clock's next ask, or
    /// the timer's next slot. `false` once told to stop.
    fn wait(&mut self, target_fps: u32) -> bool {
        self.woken_by = None;
        loop {
            if self.signal.stopped() {
                return false;
            }
            let clocked = target_fps > 0
                && self
                    .display_clock
                    .as_ref()
                    .is_some_and(vsync::DisplayClock::running);
            if clocked {
                // Drawn when a refresh calls for it; looked at again if none
                // comes, so the timer takes over once refreshes stop.
                match self.signal.wait_tick(&mut self.ticks_seen, TICK_TIMEOUT) {
                    Woken::Stop => return false,
                    Woken::TimedOut => continue,
                    Woken::Ticks(n) => {
                        self.counts.events += 1;
                        self.counts.collapsed += n - 1;
                        // Taken now, not when the frame starts: by then —
                        // the GPU waited for, the engine taken — the next
                        // refresh may have come, and the frame would move
                        // two frames on and the one after it none.
                        self.woken_by = self
                            .display_clock
                            .as_ref()
                            .and_then(vsync::DisplayClock::last_tick);
                        break;
                    }
                }
            }
            if target_fps > 0 {
                let deadline = self.cadence_anchor.unwrap_or_else(Instant::now);
                if !self.signal.sleep_until(deadline) {
                    return false;
                }
            }
            // Uncapped: straight on (the baton hand-over keeps the main
            // thread its turns).
            break;
        }
        self.woke_at = Some(Instant::now());
        true
    }

    /// One frame of the engine: what the operator and the API asked for,
    /// inputs, the mixer, every output, and now and then the state.
    fn frame(&mut self, varda: &mut VardaApp, baton_wait: Duration, gpu_wait: Duration) {
        let target_fps = varda.target_fps();
        if target_fps > 0 {
            self.check_display_clock(varda, target_fps);
            self.report_frame_clock(varda, target_fps);
        }
        if let Some(clock) = &self.display_clock {
            clock.set_by_time(varda.clock_timestamps());
        }
        let refresh = self
            .display_clock
            .as_ref()
            .filter(|clock| target_fps > 0 && clock.running());
        let start_delay = refresh.and_then(vsync::DisplayClock::since_tick);
        // On the timer — no screen refresh a whole number of which makes
        // the rate, 50 fps on a 120 Hz screen, say — the frame's slot on the
        // timer's even cadence stands in for the refresh.
        let refresh_at = match refresh {
            Some(clock) => self.woken_by.take().or_else(|| clock.last_tick()),
            None if target_fps > 0 => self.cadence_anchor,
            None => None,
        };
        varda.frame_begin(start_delay);
        varda.set_frame_refresh_at(refresh_at);
        // Where the start delay went: the callback's way to this thread, and
        // the wait for the control window to put the engine down ("ui hold").
        if let (Some(arrival), Some(woke)) = (
            refresh.and_then(vsync::DisplayClock::last_arrival),
            self.woke_at,
        ) {
            varda.frame_part("link to engine", woke.saturating_duration_since(arrival));
        }
        varda.frame_part("ui hold", baton_wait);
        // The wait for the GPU to finish the frame before (see
        // `wait_for_gpu`), made before the engine was taken.
        varda.frame_part("gpu wait", gpu_wait);
        self.counts.frames += 1;

        self.set_phase(Phase::Prepare);
        let t = Instant::now();
        varda.update_frame_timing();
        varda.update_notifications();
        varda.process_commands();
        varda.process_inputs();
        // Per-frame state advances here: after commands, before any render
        // encode. See `VardaApp::advance_frame_state`.
        varda.advance_frame_state();
        varda.frame_part("prepare", t.elapsed());

        self.set_phase(Phase::Mixer);
        let t = Instant::now();
        varda.render_mixer_frame();
        varda.frame_part("mixer", t.elapsed());

        self.set_phase(Phase::Outputs);
        let t = Instant::now();
        varda.draw_outputs();
        varda.frame_part("outputs", t.elapsed());

        self.publish_counter += 1;
        if self.publish_counter.is_multiple_of(10) {
            let link = self
                .display_clock
                .as_ref()
                .map(|clock| link_snapshot(clock, self.counts));
            varda.set_display_link_stats(link);
            self.set_phase(Phase::Publish);
            let t = Instant::now();
            varda.publish_state();
            varda.frame_part("publish", t.elapsed());
        }
        varda.frame_end();
        // Told when the GPU has finished this frame, for the wait before
        // the next (see `wait_for_gpu`).
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&done);
        varda.gpu_context().queue.on_submitted_work_done(move || {
            flag.store(true, std::sync::atomic::Ordering::Release);
        });
        self.gpu_done.push_back(done);
        self.frames_in_flight = varda.frames_in_flight();
        self.cadence_anchor = next_cadence_anchor(self.cadence_anchor, target_fps, Instant::now());
    }

    /// Follow the refresh of the screen the show goes to — the first output
    /// window's, else the control window's — when a whole number of its
    /// refreshes makes the target frame rate; looked at again every two
    /// seconds (an output opened, or moved to another screen). The screens
    /// are as the main thread last saw them.
    fn check_display_clock(&mut self, varda: &VardaApp, target_fps: u32) {
        let now = Instant::now();
        if self
            .clock_checked
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(2))
        {
            return;
        }
        self.clock_checked = Some(now);
        let monitor = varda.clock_monitor().or_else(|| varda.control_monitor());
        let wanted = monitor.and_then(|m| {
            let hz = f64::from(m.refresh_rate_millihertz()?) / 1000.0;
            let every = vsync::refreshes_per_frame(hz, target_fps)?;
            Some((display_id(&m)?, every, hz))
        });
        match (wanted, &self.display_clock) {
            (Some((id, every, hz)), Some(clock)) if clock.is(id, every, hz) => {}
            (Some((id, every, hz)), _) => {
                self.display_clock = None;
                // Told again even when screen, refreshes and rate of frames
                // are as before (a screen's Hz changed): the new clock can
                // be running before it is next reported.
                self.clock_reported = None;
                let signal = Arc::clone(&self.signal);
                self.display_clock =
                    vsync::DisplayClock::start(id, every, hz, move || signal.tick());
                match &self.display_clock {
                    Some(clock) => log::info!("Frames follow {}", clock.describe()),
                    None => log::info!("Frames follow the timer: no display clock"),
                }
            }
            (None, _) => self.display_clock = None,
        }
    }

    /// Tell the engine what starts frames, when that changes: for the
    /// outputs on that screen, and the performance figures.
    fn report_frame_clock(&mut self, varda: &mut VardaApp, target_fps: u32) {
        let now = self
            .display_clock
            .as_ref()
            .filter(|c| c.running())
            .map_or((None, 0, target_fps), |c| {
                (Some(c.display()), c.every(), target_fps)
            });
        if self.clock_reported == Some(now) {
            return;
        }
        self.clock_reported = Some(now);
        let description = match (&self.display_clock, now.0) {
            (Some(clock), Some(_)) => clock.describe(),
            _ => format!("timer at {target_fps} fps"),
        };
        varda.set_frame_clock(description, now.0);
    }
}

/// The ideal start of the next frame on the timer, after a frame that ended
/// `now`: one frame's time on from the last, keeping an even cadence — or
/// at once, after a frame that overran its slot, and from there on.
///
/// Waiting a whole frame from `now` instead turned every slightly long frame
/// into a dropped one: the state publish every tenth frame ran a little over
/// at 4K, and 60 fps settled at 60 × 10/11. `None` when uncapped.
pub(super) fn next_cadence_anchor(
    anchor: Option<Instant>,
    target_fps: u32,
    now: Instant,
) -> Option<Instant> {
    if target_fps == 0 {
        return None;
    }
    let budget = Duration::from_secs_f64(1.0 / f64::from(target_fps));
    Some(match anchor {
        Some(anchor) if anchor + budget > now => anchor + budget,
        Some(_) => now,
        None => now + budget,
    })
}

/// The display clock's figures, for the performance figures.
fn link_snapshot(
    clock: &vsync::DisplayClock,
    counts: LinkCounts,
) -> crate::engine::DisplayLinkSnapshot {
    let (callbacks, wakes) = clock.counts();
    #[allow(clippy::cast_precision_loss)]
    let ms = |ns: u64| ns as f64 / 1e6;
    crate::engine::DisplayLinkSnapshot {
        callbacks,
        wakes,
        link_events: counts.events,
        redraws: counts.frames,
        unrequested_redraws: 0,
        skipped_redraws: 0,
        collapsed: counts.collapsed,
        recent: clock
            .recent()
            .into_iter()
            .map(|tick| crate::engine::LinkTick {
                at_ms: ms(tick.at),
                shown_ms: tick.shown.map(ms),
            })
            .collect(),
    }
}

/// A screen's `CGDirectDisplayID`, where there is one (macOS).
// `None` off macOS, where there are no display ids to follow.
#[allow(clippy::unnecessary_wraps)]
fn display_id(monitor: &winit::monitor::MonitorHandle) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::MonitorHandleExtMacOS as _;
        Some(monitor.native_id())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = monitor;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn engine() -> Option<Box<VardaApp>> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        let config = crate::testing::headless_config();
        Some(Box::new(
            VardaApp::new(gpu, &config).expect("VardaApp::new"),
        ))
    }

    /// The engine thread draws frames by itself, on its timer, and hands the
    /// engine to the main thread whenever asked, between frames.
    #[test]
    fn the_engine_thread_draws_frames_and_lends_the_engine_between_them() {
        let Some(app) = engine() else {
            return;
        };
        let woken = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&woken);
        let gpu = app.gpu_context().clone();
        let thread = EngineThread::start(app, gpu, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        std::thread::sleep(Duration::from_millis(300));
        let frames = woken.load(Ordering::Relaxed);
        assert!(frames >= 5, "{frames} frames in 0.3 s at 60 fps");

        // Borrowed, the engine is the main thread's alone: no frames. (The
        // frame just put down still wakes the UI, a moment after.)
        let app = thread.baton().take().expect("the engine");
        std::thread::sleep(Duration::from_millis(20));
        let before = woken.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            woken.load(Ordering::Relaxed),
            before,
            "no frame while borrowed"
        );
        let frame_count = app.frame_stats.frame_count;
        thread.baton().put(app);

        std::thread::sleep(Duration::from_millis(100));
        let app = thread.stop().expect("the engine back");
        assert!(
            app.frame_stats.frame_count > frame_count,
            "frames again once put back"
        );
    }

    /// Stopping gives the engine back, and the thread ends.
    #[test]
    fn stopping_the_engine_thread_gives_the_engine_back() {
        let Some(app) = engine() else {
            return;
        };
        let gpu = app.gpu_context().clone();
        let thread = EngineThread::start(app, gpu, || {});
        let baton = Arc::clone(&thread.baton);
        assert!(thread.stop().is_some());
        assert!(baton.is_gone(), "the thread has ended");
        assert!(baton.take().is_none(), "nothing to take after");
    }

    /// Taken again at once (uncapped), the engine still reaches a thread
    /// waiting for it.
    #[test]
    fn a_waiting_thread_gets_its_turn_with_the_baton() {
        let Some(app) = engine() else {
            return;
        };
        let baton = Arc::new(Baton::new(app));
        let stop = Arc::new(AtomicBool::new(false));
        let greedy = {
            let (baton, stop) = (Arc::clone(&baton), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Some(app) = baton.take() else { break };
                    baton.put_and_yield(app, YIELD_PATIENCE);
                }
            })
        };
        for _ in 0..20 {
            let started = Instant::now();
            let app = baton.take().expect("a turn");
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "waited too long"
            );
            baton.put(app);
        }
        stop.store(true, Ordering::Relaxed);
        let _ = baton.close(STOP_PATIENCE);
        greedy.join().unwrap();
    }

    /// The main thread never waits on the engine for long: the engine
    /// thread may be waiting for it (a call only the main thread answers),
    /// and it would never be put down.
    #[test]
    fn a_borrow_gives_up_while_the_other_thread_keeps_the_engine() {
        let Some(app) = engine() else {
            return;
        };
        let baton = Arc::new(Baton::new(app));
        let held = baton.take().expect("the engine");
        let started = Instant::now();
        assert!(matches!(
            baton.take_within(Some(Duration::from_millis(30))),
            Taken::Late
        ));
        assert!(started.elapsed() >= Duration::from_millis(30));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "gave up in time"
        );
        baton.put(held);
        assert!(matches!(
            baton.take_within(Some(Duration::from_millis(30))),
            Taken::Engine(_)
        ));
    }

    /// Quitting does not wait forever for an engine thread that keeps the
    /// engine.
    #[test]
    fn closing_gives_up_on_an_engine_never_put_down() {
        let Some(app) = engine() else {
            return;
        };
        let baton = Baton::new(app);
        let _held = baton.take().expect("the engine");
        let started = Instant::now();
        assert!(baton.close(Duration::from_millis(30)).is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// The wait for the GPU before a frame ends when the GPU has finished,
    /// told by wgpu's work-done callback, which only a poll runs: were it
    /// never run, every frame would wait the whole patience out.
    #[test]
    fn the_wait_for_the_gpu_ends_when_the_gpu_is_done() {
        let Some(app) = engine() else {
            return;
        };
        let gpu = app.gpu_context().clone();
        let mut engine = EngineLoop::new(
            Arc::new(Signal::default()),
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
            gpu.clone(),
            Box::new(|| {}),
        );
        assert_eq!(engine.wait_for_gpu(), Duration::ZERO, "no frame yet");
        for _ in 0..3 {
            gpu.submit(std::iter::empty());
            let done = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&done);
            gpu.queue
                .on_submitted_work_done(move || flag.store(true, Ordering::Release));
            engine.gpu_done.push_back(Arc::clone(&done));
            let waited = engine.wait_for_gpu();
            assert!(done.load(Ordering::Acquire), "told the GPU finished");
            assert!(waited < GPU_PATIENCE, "{waited:?}: waited the patience out");
        }
    }

    #[test]
    fn a_tick_wakes_the_waiting_engine_thread_and_merged_ones_are_counted() {
        let signal = Signal::default();
        let mut seen = 0;
        assert_eq!(
            signal.wait_tick(&mut seen, Duration::from_millis(5)),
            Woken::TimedOut
        );
        signal.tick();
        signal.tick();
        assert_eq!(
            signal.wait_tick(&mut seen, Duration::from_millis(5)),
            Woken::Ticks(2)
        );
        signal.stop();
        assert_eq!(
            signal.wait_tick(&mut seen, Duration::from_secs(5)),
            Woken::Stop
        );
        assert!(!signal.sleep_until(Instant::now() + Duration::from_secs(5)));
    }

    #[test]
    fn the_cadence_keeps_even_steps_and_restarts_after_an_overrun() {
        let now = Instant::now();
        let budget = Duration::from_secs_f64(1.0 / 60.0);
        assert_eq!(next_cadence_anchor(None, 0, now), None, "uncapped");
        assert_eq!(next_cadence_anchor(None, 60, now), Some(now + budget));
        let ahead = now + budget * 3;
        assert_eq!(
            next_cadence_anchor(Some(ahead), 60, now),
            Some(ahead + budget)
        );
        let stale = now.checked_sub(budget * 10).unwrap();
        assert_eq!(next_cadence_anchor(Some(stale), 60, now), Some(now));
    }
}
