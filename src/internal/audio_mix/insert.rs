//! A bus's insert — an Audio Unit effect — run on a thread of its own.
//!
//! A plug-in's render is synchronous and may hang (see `au_host`), and the
//! mixer holds its lock through every block. So the mixer never calls a
//! plug-in: it hands each 10 ms block to the insert's own thread and takes
//! back the one it handed over [`INSERT_DEPTH`] blocks before, processed if
//! it came back in time and as it went in (dry) if not. The bus is that
//! much later, always — loading, playing, bypassed, failed or stalled, its
//! timing never moves. Nothing here allocates or waits on the mixer's side.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use super::core::BLOCK_FRAMES;
use super::reader::Frame;

/// How many blocks behind a bus with an insert plays: 20 ms. Its thread has
/// that long to render a block before the mixer plays it dry.
pub const INSERT_DEPTH: usize = 2;

/// One block of the mixer's.
type Block = [Frame; BLOCK_FRAMES];

/// The most delay an effect may add itself that the dry sound is held back
/// to match: a second.
const MOST_EFFECT_DELAY: usize = super::core::SAMPLE_RATE as usize;

/// The sound kept as handed over, frames: enough for the dry path to play
/// [`INSERT_DEPTH`] blocks and the effect's own delay later.
const HISTORY: usize = (INSERT_DEPTH + 1) * BLOCK_FRAMES + MOST_EFFECT_DELAY;

/// What an insert runs: a hosted Audio Unit, or a stand-in in tests.
pub trait Effect {
    /// Process `block` in place.
    ///
    /// # Errors
    ///
    /// The effect's own status: the block is then taken as it came.
    fn process(&mut self, block: &mut [Frame]) -> Result<(), i32>;

    /// `frames` passed it by (the mixer held up, or a block not handed over):
    /// what it held is from before.
    fn jumped(&mut self, _frames: u64) {}

    /// What reads its settings as a show saves them, on a thread of its own
    /// (a plug-in out of process answers over its connection, and may not).
    fn settings(&self) -> Option<ReadSettings> {
        None
    }

    /// What its window is opened with (a hosted Audio Unit's `SharedUnit`).
    fn handle(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
    }

    /// The delay it adds itself, seconds (a look-ahead, a voice isolator's
    /// analysis), on top of the insert's own [`INSERT_DEPTH`].
    fn latency_secs(&self) -> f64 {
        0.0
    }
}

/// Reads an effect's settings: `None` when it would not say.
pub type ReadSettings = Box<dyn FnMut() -> Option<Vec<u8>> + Send>;

/// How often an insert's settings are read for the show: what is set in its
/// window is saved with the show within this.
const SETTINGS_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Renders failing in a row before an insert is taken as failed: 100 ms.
const FAILURES_TO_GIVE_UP: u32 = 10;

/// Makes the effect, on the insert's own thread (a plug-in may take seconds
/// to load, and must not be loaded on the main thread).
pub type MakeEffect = Box<dyn FnOnce() -> Result<Box<dyn Effect>, String> + Send>;

/// An insert's state, as shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertState {
    Loading,
    Playing,
    /// Would not load, or its render failed: the bus plays dry.
    Failed,
}

/// What the insert's thread and the mixer tell the engine.
#[derive(Debug, Default)]
pub struct InsertStatus {
    state: AtomicU8,
    /// Why it failed, said once by its thread.
    why: Mutex<Option<String>>,
    /// The effect's last error status, 0 for none.
    last_error: AtomicI32,
    /// Blocks played dry because the processed one had not come back.
    late: AtomicU64,
    /// Its settings as last read, and how many times they changed.
    settings: Mutex<Option<Vec<u8>>>,
    settings_changes: AtomicU64,
    /// What its window is opened with, once it is made.
    handle: Mutex<Option<Arc<dyn std::any::Any + Send + Sync>>>,
    /// The effect's own delay, ms (an `f32`'s bits), once it is made.
    effect_ms: std::sync::atomic::AtomicU32,
}

impl InsertStatus {
    /// The effect's own delay, frames, up to [`MOST_EFFECT_DELAY`]: what the
    /// dry sound is held back by too.
    fn effect_frames(&self) -> usize {
        let ms = f32::from_bits(self.effect_ms.load(Ordering::Relaxed));
        if !ms.is_finite() || ms <= 0.0 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Clamped.
        let frames =
            (f64::from(ms) * f64::from(super::core::SAMPLE_RATE) / 1000.0).round() as usize;
        frames.min(MOST_EFFECT_DELAY)
    }
}

impl InsertStatus {
    pub fn state(&self) -> InsertState {
        match self.state.load(Ordering::Acquire) {
            1 => InsertState::Playing,
            2 => InsertState::Failed,
            _ => InsertState::Loading,
        }
    }

    pub fn why(&self) -> Option<String> {
        self.why.lock().ok().and_then(|w| w.clone())
    }

    /// Blocks played dry because the effect was late.
    pub fn late(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }

    /// Its settings, if they changed since `seen` changes.
    pub fn settings_since(&self, seen: &mut u64) -> Option<Vec<u8>> {
        let changes = self.settings_changes.load(Ordering::Acquire);
        if changes == *seen {
            return None;
        }
        *seen = changes;
        self.settings.lock().ok().and_then(|s| s.clone())
    }

    /// How much later the bus plays with it, ms: the handoff's 20 and the
    /// effect's own (known once it is made).
    pub fn latency_ms(&self) -> f32 {
        (INSERT_DEPTH * BLOCK_FRAMES) as f32 * 1000.0 / super::core::SAMPLE_RATE as f32
            + f32::from_bits(self.effect_ms.load(Ordering::Relaxed))
    }

    /// What its window is opened with, once its effect is made.
    pub fn handle(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.handle.lock().ok().and_then(|h| h.clone())
    }

    fn fail(&self, why: String) {
        if let Ok(mut held) = self.why.lock() {
            held.get_or_insert(why);
        }
        self.state.store(2, Ordering::Release);
    }
}

/// A bus's insert, the mixer's side.
pub struct Insert {
    to_effect: crossbeam_channel::Sender<(u64, Block)>,
    from_effect: crossbeam_channel::Receiver<(u64, Block, bool)>,
    /// The sound as handed over, by frame (block `seq` at `seq` × a block,
    /// wrapping): the dry path plays from it as late as the wet one is — two
    /// blocks and the effect's own delay — so bypassed, late or failed, the
    /// bus is heard no sooner.
    history: Box<[Frame]>,
    seq: u64,
    /// A processed block come back early, for a later turn.
    held: Option<(u64, Block, bool)>,
    /// Bypassed: the effect still runs (warm), the bus plays dry.
    pub bypass: Arc<AtomicBool>,
    /// How much of the processed sound is heard, 0–1, gliding a block.
    wet: f32,
    /// The first block handed over once it played: those before were
    /// queued while it loaded, and cannot be on time — none of them is late.
    playing_from: Option<u64>,
    pub status: Arc<InsertStatus>,
    /// Cleared when this side goes: its settings reader stops.
    alive: Arc<AtomicBool>,
}

impl Drop for Insert {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

impl Insert {
    /// An insert whose effect `make` makes, on a thread of its own named
    /// after `name`.
    pub fn new(name: &str, make: MakeEffect) -> Self {
        let (to_effect, effect_in) = crossbeam_channel::bounded::<(u64, Block)>(INSERT_DEPTH + 2);
        let (effect_out, from_effect) = crossbeam_channel::bounded(INSERT_DEPTH + 2);
        let status = Arc::new(InsertStatus::default());
        let alive = Arc::new(AtomicBool::new(true));
        let (theirs, still) = (Arc::clone(&status), Arc::clone(&alive));
        let bus = name.to_string();
        let spawned = std::thread::Builder::new()
            .name(format!("insert {name}"))
            .spawn(move || run(&bus, &theirs, &still, make, &effect_in, &effect_out));
        if let Err(e) = spawned {
            status.fail(format!("no thread for it: {e}"));
        }
        Self {
            to_effect,
            from_effect,
            history: vec![[0.0; 2]; HISTORY].into_boxed_slice(),
            seq: 0,
            held: None,
            bypass: Arc::new(AtomicBool::new(false)),
            wet: 0.0,
            playing_from: None,
            status,
            alive,
        }
    }

    /// Hand `block` (a mixer block) to the effect and replace it with the
    /// one handed over [`INSERT_DEPTH`] blocks ago, processed if it is back.
    pub fn process(&mut self, block: &mut [Frame]) {
        let Ok(input) = <&Block>::try_from(&*block) else {
            // Not a mixer block: let through as it is.
            return;
        };
        let seq = self.seq;
        self.seq += 1;
        if self.playing_from.is_none() && self.status.state() == InsertState::Playing {
            self.playing_from = Some(seq);
        }
        let at = (seq as usize).wrapping_mul(BLOCK_FRAMES) % HISTORY;
        for (i, frame) in input.iter().enumerate() {
            self.history[(at + i) % HISTORY] = *frame;
        }
        // Full: its thread is behind or hung; this block it never gets.
        let _ = self.to_effect.try_send((seq, *input));
        let Some(due) = seq.checked_sub(INSERT_DEPTH as u64) else {
            // The first blocks: what was before it, silence.
            block.fill([0.0; 2]);
            return;
        };
        let back = self.take(due);
        // Late: not back in time (a block that came back failed is not).
        if self.playing_from.is_some_and(|from| due >= from)
            && matches!(back, Back::Missing)
            && self.status.state() == InsertState::Playing
            && !self.bypass.load(Ordering::Relaxed)
        {
            self.status.late.fetch_add(1, Ordering::Relaxed);
        }
        let wet = match back {
            Back::Done(block) => Some(block),
            Back::Failed | Back::Missing => None,
        };
        let dry = self.dry(due, self.status.effect_frames());
        let dry = &dry;
        let target = if self.bypass.load(Ordering::Relaxed) {
            0.0
        } else {
            1.0
        };
        if let Some(wet) = wet {
            let n = block.len() as f32;
            let from = self.wet;
            for (i, ((out, w), d)) in block.iter_mut().zip(wet.iter()).zip(dry).enumerate() {
                let mix = from + (target - from) * (i + 1) as f32 / n;
                *out = [d[0] + (w[0] - d[0]) * mix, d[1] + (w[1] - d[1]) * mix];
            }
            self.wet = target;
        } else {
            block.copy_from_slice(dry);
            self.wet = 0.0;
        }
    }

    /// The mixer jumped over `blocks` blocks (held up): what was handed
    /// over before is stale. Its time moves on as the mixer's did, and the
    /// blocks due from before come out as silence (processed ones from then
    /// are older than any due now, and let go); its thread sees the gap and
    /// starts the effect afresh.
    pub fn skipped(&mut self, blocks: u64) {
        self.history.fill([0.0; 2]);
        self.held = None;
        while self.from_effect.try_recv().is_ok() {}
        self.seq += blocks;
    }

    /// Block `due` as it was handed over, held back `delay` more frames (the
    /// effect's own): what came before the first block, silence.
    fn dry(&self, due: u64, delay: usize) -> Block {
        let mut block = [[0.0; 2]; BLOCK_FRAMES];
        let start = (due as usize).wrapping_mul(BLOCK_FRAMES);
        for (i, frame) in block.iter_mut().enumerate() {
            if let Some(at) = (start + i).checked_sub(delay) {
                *frame = self.history[at % HISTORY];
            }
        }
        block
    }

    /// The processed block `due`, as it came back.
    fn take(&mut self, due: u64) -> Back {
        let mut found = Back::Missing;
        let mut next = self.held.take();
        loop {
            let (seq, block, ok) = match next.take() {
                Some(r) => r,
                None => match self.from_effect.try_recv() {
                    Ok(r) => r,
                    Err(_) => break,
                },
            };
            if seq < due {
                continue;
            }
            if seq == due {
                found = if ok { Back::Done(block) } else { Back::Failed };
                break;
            }
            // Ahead of its turn (it cannot be, but kept if it is).
            self.held = Some((seq, block, ok));
            break;
        }
        found
    }
}

/// A processed block, as it came back.
// On the stack, once a block: boxed, it would allocate on the mixer's thread.
#[allow(clippy::large_enum_variant)]
enum Back {
    Done(Block),
    /// Back, but its render failed.
    Failed,
    /// Not back in time.
    Missing,
}

/// The insert's thread: make the effect, then process what comes, sending
/// each block back; gone when the mixer's side is.
fn run(
    bus: &str,
    status: &Arc<InsertStatus>,
    alive: &Arc<AtomicBool>,
    make: MakeEffect,
    input: &crossbeam_channel::Receiver<(u64, Block)>,
    output: &crossbeam_channel::Sender<(u64, Block, bool)>,
) {
    let mut effect = match make() {
        Ok(effect) => effect,
        Err(why) => {
            log::warn!("Audio insert on '{bus}': {why}");
            status.fail(why);
            return;
        }
    };
    // Its own delay more than the dry sound can be held back by: heard with
    // it, then without it (bypassed, late), the bus would jump that much.
    let effect_secs = effect.latency_secs();
    if effect_secs * f64::from(super::core::SAMPLE_RATE) > MOST_EFFECT_DELAY as f64 {
        let why = format!(
            "its own delay ({:.0} ms) is more than a second",
            effect_secs * 1000.0
        );
        log::warn!("Audio insert on '{bus}': {why}");
        status.fail(why);
        return;
    }
    #[allow(clippy::cast_possible_truncation)] // Milliseconds.
    let effect_ms = (effect_secs * 1000.0) as f32;
    status
        .effect_ms
        .store(effect_ms.to_bits(), Ordering::Relaxed);
    if let Ok(mut handle) = status.handle.lock() {
        *handle = effect.handle();
    }
    if let Some(read) = effect.settings() {
        read_settings(Arc::clone(status), Arc::clone(alive), read);
    }
    status.state.store(1, Ordering::Release);
    let mut next = None;
    let mut failing = 0u32;
    while let Ok((seq, mut block)) = input.recv() {
        // A gap: the mixer jumped, or a block did not fit while it was
        // behind. What the effect held is from before.
        if let Some(gap) = next
            .and_then(|n: u64| seq.checked_sub(n))
            .filter(|g| *g > 0)
        {
            effect.jumped(gap * BLOCK_FRAMES as u64);
        }
        next = Some(seq + 1);
        let ok = match effect.process(&mut block) {
            Ok(()) => {
                failing = 0;
                true
            }
            Err(code) => {
                failing += 1;
                if status.last_error.swap(code, Ordering::Relaxed) != code {
                    log::warn!("Audio insert on '{bus}': its render failed ({code}): played dry");
                }
                if failing >= FAILURES_TO_GIVE_UP {
                    // Its host gone, most likely: no more of it; the bus is
                    // heard without it, its timing as it was.
                    log::warn!("Audio insert on '{bus}': taken out, its render failing");
                    status.fail(format!(
                        "its render failed {failing} times in a row (error {code})"
                    ));
                    return;
                }
                false
            }
        };
        // Full: the mixer is not reading (held up): this one goes.
        let _ = output.try_send((seq, block, ok));
    }
}

/// Read an effect's settings every [`SETTINGS_EVERY`] on a thread of its own
/// while its insert lives — never the render thread or the main one: a
/// plug-in that will not answer holds up only this.
fn read_settings(status: Arc<InsertStatus>, alive: Arc<AtomicBool>, mut read: ReadSettings) {
    let spawned = std::thread::Builder::new()
        .name("insert settings".into())
        .spawn(move || {
            let step = std::time::Duration::from_millis(100);
            let mut waited = SETTINGS_EVERY;
            while alive.load(Ordering::Acquire) {
                if waited >= SETTINGS_EVERY {
                    waited = std::time::Duration::ZERO;
                    if let Some(now) = read() {
                        let changed = status.settings.lock().is_ok_and(|mut held| {
                            let changed = held.as_ref() != Some(&now);
                            if changed {
                                *held = Some(now);
                            }
                            changed
                        });
                        if changed {
                            status.settings_changes.fetch_add(1, Ordering::Release);
                        }
                    }
                }
                std::thread::sleep(step);
                waited += step;
            }
        });
    if let Err(e) = spawned {
        log::warn!("Audio insert: its settings will not be saved (no thread: {e})");
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, Insert, InsertState, INSERT_DEPTH};
    use crate::internal::audio_mix::core::BLOCK_FRAMES;
    use crate::internal::audio_mix::reader::Frame;
    use std::time::Duration;

    struct Gain(f32, Duration);

    impl Effect for Gain {
        fn process(&mut self, block: &mut [Frame]) -> Result<(), i32> {
            std::thread::sleep(self.1);
            for f in block.iter_mut() {
                *f = [f[0] * self.0, f[1] * self.0];
            }
            Ok(())
        }
    }

    struct Fails;

    impl Effect for Fails {
        fn process(&mut self, block: &mut [Frame]) -> Result<(), i32> {
            block.fill([9.0; 2]);
            Err(-1)
        }
    }

    /// Block `i`: every sample at `i / 100`.
    fn block(i: usize) -> Vec<Frame> {
        vec![[i as f32 / 100.0; 2]; BLOCK_FRAMES]
    }

    /// Run `blocks` blocks through, giving its thread `pace` between, and
    /// return the first sample of each that came out.
    fn run(insert: &mut Insert, blocks: usize, pace: Duration) -> Vec<f32> {
        (0..blocks)
            .map(|i| {
                let mut b = block(i);
                insert.process(&mut b);
                std::thread::sleep(pace);
                b[BLOCK_FRAMES - 1][0]
            })
            .collect()
    }

    fn loaded(insert: &Insert) {
        let end = std::time::Instant::now() + Duration::from_secs(2);
        while insert.status.state() == InsertState::Loading && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The blocks due while it loads, or before its first comes back, are
    /// not late: a fresh insert shows none.
    #[test]
    fn a_fresh_insert_is_not_late() {
        let mut insert = Insert::new(
            "gain",
            Box::new(|| {
                std::thread::sleep(Duration::from_millis(30));
                Ok(Box::new(Gain(2.0, Duration::ZERO)))
            }),
        );
        run(&mut insert, 12, Duration::from_millis(5));
        assert_eq!(insert.status.state(), InsertState::Playing);
        assert_eq!(insert.status.late(), 0);
    }

    /// Processed and exactly two blocks later, every block.
    #[test]
    fn an_insert_plays_its_effect_two_blocks_later() {
        let mut insert = Insert::new("gain", Box::new(|| Ok(Box::new(Gain(2.0, Duration::ZERO)))));
        loaded(&insert);
        let out = run(&mut insert, 12, Duration::from_millis(3));
        assert_eq!(&out[..INSERT_DEPTH], &[0.0, 0.0], "silence before it");
        for (i, v) in out.iter().enumerate().skip(INSERT_DEPTH) {
            let want = 2.0 * (i - INSERT_DEPTH) as f32 / 100.0;
            assert!((v - want).abs() < 1e-6, "block {i}: {v} not {want}");
        }
        assert_eq!(insert.status.late(), 0);
    }

    /// An effect too slow for its turn: that block plays dry, on time, and
    /// is counted; the mixer never waits.
    #[test]
    fn a_late_effect_is_played_dry_on_time() {
        let mut insert = Insert::new(
            "slow",
            Box::new(|| Ok(Box::new(Gain(2.0, Duration::from_millis(60))))),
        );
        loaded(&insert);
        let start = std::time::Instant::now();
        let out = run(&mut insert, 8, Duration::from_millis(1));
        assert!(
            start.elapsed() < Duration::from_millis(40),
            "never waited: {:?}",
            start.elapsed()
        );
        for (i, v) in out.iter().enumerate().skip(INSERT_DEPTH) {
            let dry = (i - INSERT_DEPTH) as f32 / 100.0;
            assert!((v - dry).abs() < 1e-6, "block {i}: dry, on time");
        }
        assert!(insert.status.late() >= 5, "{}", insert.status.late());
    }

    /// An effect whose render fails: its output is not heard, the dry
    /// block is.
    #[test]
    fn a_failing_render_is_played_dry() {
        let mut insert = Insert::new("fails", Box::new(|| Ok(Box::new(Fails))));
        loaded(&insert);
        let out = run(&mut insert, 6, Duration::from_millis(3));
        for (i, v) in out.iter().enumerate().skip(INSERT_DEPTH) {
            assert!(
                (v - (i - INSERT_DEPTH) as f32 / 100.0).abs() < 1e-6,
                "block {i}: {v}"
            );
        }
    }

    /// An effect that will not load: failed, said why, and the bus plays
    /// dry, still two blocks later.
    #[test]
    fn an_effect_that_will_not_load_plays_dry() {
        let mut insert = Insert::new("missing", Box::new(|| Err("not installed here".into())));
        loaded(&insert);
        assert_eq!(insert.status.state(), InsertState::Failed);
        assert_eq!(insert.status.why().as_deref(), Some("not installed here"));
        let out = run(&mut insert, 5, Duration::ZERO);
        assert!((out[4] - 0.02).abs() < 1e-6, "{out:?}");
    }

    /// Bypassed: the dry block, faded to over a block, the effect still fed.
    #[test]
    fn a_bypassed_insert_fades_to_dry() {
        let mut insert = Insert::new("gain", Box::new(|| Ok(Box::new(Gain(2.0, Duration::ZERO)))));
        loaded(&insert);
        run(&mut insert, 6, Duration::from_millis(3));
        insert
            .bypass
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut b = block(6);
        insert.process(&mut b);
        // From wet (2 × 0.04) to dry (0.04) across the block.
        assert!((b[0][0] - 0.08).abs() < 0.001 && (b[BLOCK_FRAMES - 1][0] - 0.04).abs() < 1e-6);
        std::thread::sleep(Duration::from_millis(3));
        // Blocks 5 and 6 come out next: dry.
        let out = run(&mut insert, 2, Duration::from_millis(3));
        assert!(
            (out[0] - 0.05).abs() < 1e-6 && (out[1] - 0.06).abs() < 1e-6,
            "dry: {out:?}"
        );
        assert_eq!(insert.status.late(), 0, "bypassed is not late");
    }

    /// An effect whose render keeps failing is taken as failed — said why,
    /// not shown playing — and the bus goes on without it; one that fails
    /// now and then is not counted late.
    #[test]
    fn an_effect_failing_again_and_again_is_failed() {
        let mut insert = Insert::new("fails", Box::new(|| Ok(Box::new(Fails))));
        loaded(&insert);
        run(&mut insert, 16, Duration::from_millis(2));
        assert_eq!(insert.status.state(), InsertState::Failed);
        assert!(
            insert.status.why().is_some_and(|w| w.contains("10 times")),
            "{:?}",
            insert.status.why()
        );
        assert_eq!(insert.status.late(), 0, "failed, not late");
    }

    /// After the mixer jumps (held up), the blocks due from before come out
    /// as silence, not replayed, and the effect is told of the gap.
    #[test]
    fn a_jump_drops_what_was_held_and_tells_the_effect() {
        struct Counts(std::sync::Arc<std::sync::atomic::AtomicU64>);
        impl Effect for Counts {
            fn process(&mut self, _: &mut [Frame]) -> Result<(), i32> {
                Ok(())
            }
            fn jumped(&mut self, frames: u64) {
                self.0
                    .fetch_add(frames, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let jumped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let told = std::sync::Arc::clone(&jumped);
        let mut insert = Insert::new("counts", Box::new(move || Ok(Box::new(Counts(told)))));
        loaded(&insert);
        run(&mut insert, 6, Duration::from_millis(2));
        insert.skipped(5);
        let mut b = block(50);
        insert.process(&mut b);
        assert!(
            b.iter().all(|f| *f == [0.0, 0.0]),
            "before the jump: silence"
        );
        let mut b = block(51);
        insert.process(&mut b);
        assert!(b.iter().all(|f| *f == [0.0, 0.0]));
        std::thread::sleep(Duration::from_millis(3));
        let out = run(&mut insert, 1, Duration::from_millis(2));
        assert!(
            (out[0] - 0.50).abs() < 1e-6,
            "then the sound after it: {out:?}"
        );
        assert_eq!(
            jumped.load(std::sync::atomic::Ordering::Relaxed),
            5 * BLOCK_FRAMES as u64
        );
    }

    /// An effect's settings are read on their own thread, from the first
    /// moment, and told once each time they change; the reading stops when
    /// the insert goes.
    #[test]
    fn an_effects_settings_are_read_while_it_lives() {
        struct Set(std::sync::Arc<std::sync::atomic::AtomicU64>);
        impl Effect for Set {
            fn process(&mut self, _: &mut [Frame]) -> Result<(), i32> {
                Ok(())
            }
            fn settings(&self) -> Option<super::ReadSettings> {
                let reads = std::sync::Arc::clone(&self.0);
                Some(Box::new(move || {
                    let n = reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Some(vec![u8::from(n > 0)])
                }))
            }
        }
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let theirs = std::sync::Arc::clone(&reads);
        let insert = Insert::new("set", Box::new(move || Ok(Box::new(Set(theirs)))));
        loaded(&insert);
        std::thread::sleep(Duration::from_millis(150));
        let mut seen = 0;
        assert_eq!(
            insert.status.settings_since(&mut seen),
            Some(vec![0]),
            "read at once"
        );
        assert_eq!(insert.status.settings_since(&mut seen), None, "told once");
        let status = std::sync::Arc::clone(&insert.status);
        drop(insert);
        let at_drop = reads.load(std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(super::SETTINGS_EVERY + Duration::from_millis(300));
        assert!(
            reads.load(std::sync::atomic::Ordering::Relaxed) <= at_drop + 1,
            "stopped"
        );
        assert!(status.settings_since(&mut seen).is_none());
    }

    /// An effect with a delay of its own: the dry sound is held back as much,
    /// so bypassed (or late, or failed) the bus is heard exactly as late as
    /// with it — the timing never moves.
    #[test]
    fn the_dry_sound_is_as_late_as_an_effect_with_a_delay() {
        /// Puts its sound 240 frames (5 ms) later, and says so.
        struct Delayed(std::collections::VecDeque<Frame>);
        impl Effect for Delayed {
            fn process(&mut self, block: &mut [Frame]) -> Result<(), i32> {
                for frame in block.iter_mut() {
                    self.0.push_back(*frame);
                    *frame = self.0.pop_front().unwrap_or([0.0; 2]);
                }
                Ok(())
            }
            fn latency_secs(&self) -> f64 {
                0.005
            }
        }
        let impulse_at = |bypass: bool| {
            let mut insert = Insert::new(
                "delayed",
                Box::new(|| {
                    Ok(Box::new(Delayed(
                        std::iter::repeat_n([0.0; 2], 240).collect(),
                    )))
                }),
            );
            insert
                .bypass
                .store(bypass, std::sync::atomic::Ordering::Relaxed);
            loaded(&insert);
            let mut out = Vec::new();
            for i in 0..6 {
                let mut b = vec![[0.0_f32; 2]; BLOCK_FRAMES];
                if i == 0 {
                    b[0] = [1.0, 1.0];
                }
                insert.process(&mut b);
                std::thread::sleep(Duration::from_millis(3));
                out.extend(b);
            }
            out.iter().position(|f| f[0] > 0.5)
        };
        let wet = impulse_at(false).expect("heard with it");
        assert_eq!(wet, INSERT_DEPTH * BLOCK_FRAMES + 240);
        assert_eq!(impulse_at(true), Some(wet), "bypassed: as late");
    }

    /// An effect saying it delays its sound more than a second is not used:
    /// the dry sound could not be held back as much.
    #[test]
    fn an_effect_with_too_long_a_delay_is_not_used() {
        struct Slow;
        impl Effect for Slow {
            fn process(&mut self, _: &mut [Frame]) -> Result<(), i32> {
                Ok(())
            }
            fn latency_secs(&self) -> f64 {
                2.0
            }
        }
        let insert = Insert::new("slow", Box::new(|| Ok(Box::new(Slow))));
        loaded(&insert);
        assert_eq!(insert.status.state(), InsertState::Failed);
        assert!(insert
            .status
            .why()
            .is_some_and(|w| w.contains("more than a second")));
    }

    /// An effect that hangs: the mixer goes on, dry, never held up.
    #[test]
    fn a_hung_effect_never_holds_up_the_mixer() {
        struct Hangs;
        impl Effect for Hangs {
            fn process(&mut self, _: &mut [Frame]) -> Result<(), i32> {
                std::thread::sleep(Duration::from_secs(3600));
                Ok(())
            }
        }
        let mut insert = Insert::new("hangs", Box::new(|| Ok(Box::new(Hangs))));
        loaded(&insert);
        let start = std::time::Instant::now();
        let out = run(&mut insert, 50, Duration::ZERO);
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "{:?}",
            start.elapsed()
        );
        assert!((out[49] - 0.47).abs() < 1e-6, "dry");
    }
}
