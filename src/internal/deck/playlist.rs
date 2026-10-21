//! A playlist: clips and pictures shown one after another in one layer, as
//! vMix's List input. A picture stays up for the playlist's picture time, a
//! clip plays to its end, and each gives way to the next by a cut, a fade or
//! a slide. See [`crate::engine::value::playlist`].
//!
//! Each item is a deck of its own — a clip or a picture, as if added alone —
//! drawn at the playlist's size and mixed into the playlist's picture. Only
//! the item on screen and the one after it are open: the next is loaded on a
//! thread while the one before it plays, so moving on never waits on a file,
//! and one let go is closed on a thread too (a clip's decoder takes a moment
//! to stop).

use std::sync::mpsc;
use std::time::Instant;

use super::media::MediaKind;
use super::{Deck, ScalingMode};
use crate::engine::value::playlist::{
    PlaylistItem, PlaylistSettings, PlaylistSnapshot, PlaylistTransition,
};
use crate::renderer::playlist_mix::{Mix, PlaylistMixPipeline};
use crate::renderer::GpuContext;

/// The longest step time takes between two looks, so a stall (a dragged
/// window, a slow frame) does not jump a picture's time.
const MAX_STEP: f64 = 0.25;
/// A clip is on screen at least this long before its end counts: just after
/// it is rewound, the decode thread may not yet have said it left the end.
const CLIP_SETTLE: f64 = 0.3;
/// How much of a frame's time a slide is blurred over — a film camera's
/// half-open shutter.
const SHUTTER: f64 = 0.5;
/// Frames a second assumed until the renderer says.
const DEFAULT_FPS: u32 = 60;

/// An item open as a deck.
struct Open {
    /// Its place in the list. For an item taken out of the list while on
    /// screen, the place of the item after it.
    index: usize,
    /// Taken out of the list while on screen.
    gone: bool,
    path: String,
    deck: Box<Deck>,
    /// A clip, which plays to its end, not a picture.
    clip: bool,
    /// Seconds on screen, whole and playing.
    held: f64,
}

/// An item being opened on a thread.
struct Opening {
    index: usize,
    path: String,
    done: mpsc::Receiver<anyhow::Result<Deck>>,
}

pub struct Playlist {
    context: GpuContext,
    /// The size items are drawn at: the master's.
    size: (u32, u32),
    items: Vec<PlaylistItem>,
    settings: PlaylistSettings,
    scaling_mode: ScalingMode,
    playing: bool,
    shown: Option<Open>,
    next: Option<Open>,
    /// Seconds into the transition from `shown` to `next`.
    transition: Option<f64>,
    /// Show `next` as soon as it is open, not when `shown` is done: an item
    /// picked by hand.
    jump: bool,
    opening: Option<Opening>,
    /// Items that would not open, by path: skipped until the list changes.
    failed: Vec<String>,
    last_step: Option<Instant>,
    /// When the refresh that called for the frame being drawn came, when a
    /// screen's refresh starts frames: the next step moves on to it rather
    /// than to when the frame started (see [`Self::frame_refresh_at`]).
    refresh_at: Option<Instant>,
    /// Each frame is held on screen a frame's time (see [`Self::frame_held`]).
    held: bool,
    /// Frames a second the renderer shows (0 until it says).
    output_fps: std::sync::atomic::AtomicU32,
    /// Wall-clock time not yet counted in whole frames (see [`Self::steady`]).
    clock_debt: f64,
    mix: PlaylistMixPipeline,
}

impl Playlist {
    pub fn new(
        context: &GpuContext,
        items: Vec<PlaylistItem>,
        settings: PlaylistSettings,
        (width, height): (u32, u32),
    ) -> Self {
        Self {
            context: context.clone(),
            size: (width.max(1), height.max(1)),
            items,
            settings: settings.clamped(),
            // A new picture fits the frame whole, as vMix adds it.
            scaling_mode: ScalingMode::Fit,
            playing: false,
            shown: None,
            next: None,
            transition: None,
            jump: false,
            opening: None,
            failed: Vec::new(),
            last_step: None,
            refresh_at: None,
            held: false,
            output_fps: std::sync::atomic::AtomicU32::new(0),
            clock_debt: 0.0,
            mix: PlaylistMixPipeline::new(&context.device, context.compositing_format),
        }
    }

    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    pub fn settings(&self) -> PlaylistSettings {
        self.settings
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// The clip on screen, for the input's transport. Pictures have no
    /// playhead. The playlist owns play/pause, including at a clip's end.
    pub fn playback_snapshot(&self) -> Option<crate::video::PlaybackSnapshot> {
        let mut snapshot = self.shown.as_ref()?.deck.playback_snapshot()?;
        snapshot.playing = self.playing;
        Some(snapshot)
    }

    /// Scrub the clip on screen without starting or pausing the playlist.
    /// A hand on its playhead cancels any pending change to another item.
    pub fn seek(&mut self, position_secs: f64) -> bool {
        if !position_secs.is_finite() {
            return false;
        }
        let Some(open) = self.shown.as_mut().filter(|open| open.clip) else {
            return false;
        };
        let Some(playback) = open.deck.playback_snapshot() else {
            return false;
        };
        let position = position_secs.clamp(
            playback.in_point,
            playback.effective_out().max(playback.in_point),
        );
        open.deck.video_seek(position);
        // Ignore the decoder's old end snapshot until the seek is processed.
        open.held = 0.0;
        self.last_step = None;
        let changing_item = self.transition.is_some() || self.jump;
        self.transition = None;
        self.jump = false;
        if changing_item {
            self.drop_next();
        }
        true
    }

    /// The renderer shows `fps` frames a second (0: as fast as it can).
    pub fn set_output_fps(&self, fps: u32) {
        self.output_fps
            .store(fps, std::sync::atomic::Ordering::Relaxed);
    }

    fn frame_secs(&self) -> Option<f64> {
        match self.output_fps.load(std::sync::atomic::Ordering::Relaxed) {
            0 => None,
            fps => Some(1.0 / f64::from(fps)),
        }
    }

    /// How far the transition has got, 0 to 1, while one runs.
    pub fn transition_progress(&self) -> Option<f32> {
        self.transition.map(|t| self.settings.progress(t))
    }

    /// `raw` seconds of wall clock, as whole frames at the rate frames are
    /// shown; the rest is carried to the next step.
    ///
    /// Frames are drawn on a timer that wakes a few milliseconds early or
    /// late — later still when the control window took long to lay out —
    /// but reach the screen at its steady refresh. Moved on by the clock at
    /// each drawing, a slide lands a few pixels short or long of where that
    /// steady refresh puts it: a micro-stutter. Moved on by whole frames, it
    /// glides; a frame truly missed still counts two.
    fn steady(&mut self, raw: f64) -> f64 {
        let Some(frame) = self.frame_secs() else {
            return raw;
        };
        let owed = self.clock_debt + raw;
        if self.held {
            // One frame a frame, unless more than a frame off the clock.
            let frames = if owed < -frame {
                0.0
            } else if owed > 2.0 * frame {
                (owed / frame).round()
            } else {
                1.0
            };
            self.clock_debt = (owed - frames * frame).clamp(-2.0 * frame, 2.0 * frame);
            return frames * frame;
        }
        let frames = (owed / frame).round();
        self.clock_debt = (owed - frames * frame).clamp(-frame, frame);
        frames * frame
    }

    pub fn scaling_mode(&self) -> ScalingMode {
        self.scaling_mode
    }

    /// Every item's deck is scaled this way.
    pub fn set_scaling_mode(&mut self, mode: ScalingMode) {
        self.scaling_mode = mode;
        for open in self.shown.iter_mut().chain(self.next.iter_mut()) {
            open.deck.set_scaling_mode(mode);
        }
    }

    pub fn set_settings(&mut self, settings: PlaylistSettings) {
        let looped = self.settings.loop_list;
        self.settings = settings.clamped();
        // What comes after the last item changed: what was opened for it no
        // longer is.
        if looped != self.settings.loop_list && self.transition.is_none() && !self.jump {
            self.drop_next();
        }
    }

    /// A new list: items added, taken out or moved. What is on screen stays
    /// there and the list plays on from it.
    pub fn set_items(&mut self, items: Vec<PlaylistItem>) {
        self.items = items;
        self.failed.clear();
        if self.items.is_empty() {
            self.jump = false;
            self.transition = None;
            self.drop_next();
            if let Some(open) = self.shown.take() {
                let_go(open.deck);
            }
            return;
        }
        if let Some(open) = &mut self.shown {
            reindex(open, &self.items);
        }
        if self.transition.is_some() {
            if let Some(open) = &mut self.next {
                reindex(open, &self.items);
            }
        } else {
            self.jump = false;
            self.drop_next();
        }
    }

    /// Play or stop. With Auto Next off, replay the current item after it
    /// ends. Otherwise restart a finished, non-looping list from the top.
    pub fn set_playing(&mut self, playing: bool) {
        if playing == self.playing {
            return;
        }
        self.playing = playing;
        self.last_step = None;
        if playing && !self.settings.auto_next {
            if let Some(open) = self.shown.as_ref().filter(|open| self.done(open, 0.0)) {
                if open.clip {
                    open.deck.video_seek(0.0);
                }
                if let Some(open) = &mut self.shown {
                    open.held = 0.0;
                }
            }
        }
        if playing && self.settings.auto_next && self.finished() {
            self.show_item(0);
            return;
        }
        if let Some(open) = &self.shown {
            open.deck.video_set_playing(playing);
        }
        if let Some(next) = &self.next {
            // Preloaded clips stay cued until their transition starts.
            next.deck
                .video_set_playing(playing && self.transition.is_some());
        }
    }

    /// Show item `index` now — with the transition while playing, at once
    /// while stopped — and play on from it.
    pub fn show_item(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        if self.transition.is_some() {
            self.finish_transition();
        }
        self.drop_next();
        self.failed.retain(|p| *p != self.items[index].path);
        self.jump = true;
        self.open(index);
    }

    /// At the last item of a list that does not loop, done with it.
    fn finished(&self) -> bool {
        self.shown.as_ref().is_some_and(|open| {
            self.following(open).is_none() && self.transition.is_none() && self.done(open, 0.0)
        })
    }

    /// Once a frame: take what has opened, move time on, and bring each clip's
    /// newest frame to its texture.
    pub fn tick(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.step(Instant::now());
        for open in self.shown.iter_mut().chain(self.next.iter_mut()) {
            if let Err(e) = open.deck.update_video_frame(encoder) {
                log::warn!("Playlist item '{}': {e}", open.path);
            }
        }
    }

    /// The frame about to be drawn was called for by the screen's refresh
    /// at `at`. A frame can start well after its refresh — held up by the
    /// frame before — and still be shown at the refresh it was meant for;
    /// moved on by when it started, a slide went two frames on and then
    /// none, a hitch on screen. Moved on by its refresh, it goes one.
    pub fn frame_refresh_at(&mut self, at: Option<Instant>) {
        self.refresh_at = at;
    }

    /// A screen's refresh starts each frame, so each is shown exactly once
    /// in turn, however unevenly they were started. Then each moves the
    /// transition on one frame, as it will be seen; only when that falls more
    /// than a frame behind the clock, or ahead of it, does one move two, or
    /// none.
    pub fn frame_held(&mut self, held: bool) {
        self.held = held;
    }

    /// Move the playlist on to `now` — or to the refresh the frame was
    /// called for, when that is known.
    pub fn step(&mut self, now: Instant) {
        let now = self.refresh_at.take().unwrap_or(now);
        let raw = self.last_step.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f64()
        });
        if self.last_step.is_none() {
            self.clock_debt = 0.0;
        }
        let dt = self.steady(raw.min(MAX_STEP));
        self.last_step = Some(now);
        self.take_opened();

        if self.shown.is_none() {
            if self.opening.is_none() {
                if let Some(first) = self.openable_from(0) {
                    self.open(first);
                }
            }
            return;
        }

        // Moving on to an item picked by hand does not wait for time.
        if self.jump && self.next.is_some() && self.transition.is_none() {
            self.start_transition();
        }
        if !self.playing {
            // Stopped, a pick shows at once.
            if self.transition.is_some() {
                self.finish_transition();
            }
            return;
        }
        if let Some(elapsed) = &mut self.transition {
            *elapsed += dt;
            if *elapsed >= self.settings.transition_time() {
                self.finish_transition();
            }
            return;
        }

        // Waiting for an item picked by hand to open: nothing else moves on
        // (the end of the list must not stop what the pick started).
        if self.jump {
            return;
        }
        let Some(open) = &mut self.shown else {
            return;
        };
        open.held += dt;
        let lead = self.settings.transition_time();
        let Some(open) = self.shown.as_ref() else {
            return;
        };
        if !self.settings.auto_next {
            // A manual pick above still works, but automatic playback ends
            // on this item, without starting a transition early.
            if self.done(open, 0.0) {
                self.set_playing(false);
            }
            self.open_following();
            return;
        }
        let following = self.following(open);
        if self.done(open, lead) {
            match following {
                // One item that loops: a clip starts again, a picture stays.
                Some(index) if index == open.index && !open.gone => {
                    if open.clip && self.done(open, 0.0) {
                        open.deck.video_seek(0.0);
                        open.deck.video_set_playing(true);
                        if let Some(open) = &mut self.shown {
                            open.held = 0.0;
                        }
                    }
                }
                Some(_) if self.next.is_some() => self.start_transition(),
                // Still opening: moved on to once it is.
                Some(_) => {}
                // The end of a list that does not loop: it stops there once
                // the last item is done.
                None => {
                    if self.done(open, 0.0) {
                        self.playing = false;
                    }
                }
            }
        }
        self.open_following();
    }

    /// Whether `open` has had its time, `lead` seconds early for a clip (so
    /// a transition ends as the clip does).
    fn done(&self, open: &Open, lead: f64) -> bool {
        if !open.clip {
            return open.held >= self.settings.picture_secs;
        }
        if open.held < CLIP_SETTLE {
            return false;
        }
        let Some(playback) = open.deck.playback_snapshot() else {
            return true;
        };
        let end = playback.effective_out();
        // Pausing clears the decoder's transient reached_end flag. Its
        // stored position still identifies an ended clip when Play replays it.
        if playback.reached_end || (end > 0.0 && playback.position >= end) {
            return true;
        }
        lead > 0.0 && end > lead && playback.position_now() >= end - lead
    }

    /// The item after `open`, if any: the next in the list, the first after
    /// the last when the list loops. For an item taken out of the list, the
    /// one that took its place.
    fn following(&self, open: &Open) -> Option<usize> {
        let len = self.items.len();
        let after = if open.gone {
            open.index
        } else {
            open.index + 1
        };
        if after < len {
            Some(after)
        } else if self.settings.loop_list && len > 0 {
            Some(0)
        } else {
            None
        }
    }

    /// The first item from `index` on (wrapping round) that has not failed.
    fn openable_from(&self, index: usize) -> Option<usize> {
        let len = self.items.len();
        (0..len)
            .map(|i| (index + i) % len)
            .find(|&i| !self.failed.contains(&self.items[i].path))
    }

    /// Start opening the item after the one on screen, if it is not already.
    fn open_following(&mut self) {
        if self.next.is_some() || self.opening.is_some() || self.transition.is_some() {
            return;
        }
        let Some(open) = &self.shown else {
            return;
        };
        let Some(following) = self.following(open) else {
            return;
        };
        let Some(index) = self.openable_from(following) else {
            return;
        };
        // One item that loops is rewound, not opened twice.
        if index == open.index && !open.gone {
            return;
        }
        self.open(index);
    }

    /// Open item `index` on a thread.
    fn open(&mut self, index: usize) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let path = item.path.clone();
        let (tx, done) = mpsc::channel();
        let context = self.context.clone();
        let (width, height) = self.size;
        let scaling_mode = self.scaling_mode;
        let file = path.clone();
        let spawned = std::thread::Builder::new()
            .name("playlist-open".into())
            .spawn(move || {
                let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    open_item(&context, &file, (width, height), scaling_mode)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("opening it crashed")));
                let _ = tx.send(opened);
            });
        if let Err(e) = spawned {
            log::warn!("Playlist: could not start opening '{path}': {e}");
            return;
        }
        self.opening = Some(Opening { index, path, done });
    }

    /// Take the item opened, if it has; skip one that would not open.
    fn take_opened(&mut self) {
        let Some(opening) = &self.opening else {
            return;
        };
        let result = match opening.done.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("opening it stopped")),
        };
        let opening = self.opening.take().expect("opening");
        // The list changed while it opened.
        if self.items.get(opening.index).map(|i| &i.path) != Some(&opening.path) {
            if let Ok(deck) = result {
                let_go(Box::new(deck));
            }
            return;
        }
        match result {
            Ok(deck) => {
                let clip = deck.playback_snapshot().is_some();
                let open = Open {
                    index: opening.index,
                    gone: false,
                    path: opening.path,
                    deck: Box::new(deck),
                    clip,
                    held: 0.0,
                };
                if self.shown.is_none() {
                    self.jump = false;
                    if open.clip && self.playing {
                        open.deck.video_set_playing(true);
                    }
                    self.shown = Some(open);
                } else {
                    self.next = Some(open);
                }
            }
            Err(e) => {
                log::warn!("Playlist: '{}' skipped: {e:#}", opening.path);
                self.failed.push(opening.path);
                // A pick that failed shows nothing new.
                self.jump = false;
            }
        }
    }

    fn start_transition(&mut self) {
        let Some(next) = &self.next else {
            return;
        };
        if next.clip && self.playing {
            next.deck.video_set_playing(true);
        }
        self.transition = Some(0.0);
        if self.settings.transition_time() <= 0.0 {
            self.finish_transition();
        }
    }

    fn finish_transition(&mut self) {
        self.transition = None;
        self.jump = false;
        let Some(mut next) = self.next.take() else {
            return;
        };
        next.held = 0.0;
        if next.clip {
            next.deck.video_set_playing(self.playing);
        }
        if let Some(old) = self.shown.replace(next) {
            let_go(old.deck);
        }
    }

    fn drop_next(&mut self) {
        if let Some(open) = self.next.take() {
            let_go(open.deck);
        }
        // Its thread finishes on its own; what it opens is let go then.
        self.opening = None;
    }

    /// Draw the playlist's picture into `output`: the item on screen, or it
    /// giving way to the next.
    pub fn render(
        &mut self,
        audio_data: &crate::audio::AudioData,
        modulation: &crate::modulation::ModulationEngine,
        param_prefix: &str,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        output: &wgpu::TextureView,
    ) {
        let context = self.context.clone();
        let progress = self.transition.map(|t| self.settings.progress(t));
        // The ground a slide covers while the shutter is open this frame.
        let blur = self.transition.map_or(0.0, |t| {
            let open = self.frame_secs().unwrap_or(1.0 / f64::from(DEFAULT_FPS)) * SHUTTER;
            self.settings.progress(t + open / 2.0)
                - self.settings.progress((t - open / 2.0).max(0.0))
        });
        let mut draw = |open: &mut Open| {
            if let Err(e) = open.deck.render_with_prefix(
                &context,
                audio_data,
                modulation,
                param_prefix,
                cmd_buffers,
                None,
            ) {
                log::warn!("Playlist item '{}': {e}", open.path);
            }
        };
        if let Some(open) = &mut self.shown {
            draw(open);
        }
        let mix = match (progress, &mut self.next) {
            (Some(t), Some(next)) => {
                draw(next);
                match self.settings.transition {
                    PlaylistTransition::Cut => Mix::Shown,
                    PlaylistTransition::Fade => Mix::Fade(t),
                    PlaylistTransition::Slide => {
                        Mix::Slide(t, self.settings.slide_from.offset(), blur)
                    }
                }
            }
            _ => Mix::Shown,
        };
        let next_view = match mix {
            Mix::Shown => None,
            _ => self.next.as_ref().map(|o| o.deck.output_view()),
        };
        self.mix.draw(
            &context.device,
            &context.queue,
            cmd_buffers,
            mix,
            self.shown.as_ref().map(|o| o.deck.output_view()),
            next_view,
            output,
            self.size,
        );
    }

    /// The master's size changed: items are drawn at the new one.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.size = (width.max(1), height.max(1));
        let context = self.context.clone();
        for open in self.shown.iter_mut().chain(self.next.iter_mut()) {
            open.deck.resize(&context, self.size.0, self.size.1);
        }
    }

    /// The items' clips: kept from decoding while the playlist's own deck is
    /// (see /spec/deck-residency.md), and at the rate frames are shown.
    pub fn for_each_deck(&self, mut f: impl FnMut(&Deck)) {
        for open in self.shown.iter().chain(self.next.iter()) {
            f(&open.deck);
        }
    }

    pub fn for_each_deck_mut(&mut self, mut f: impl FnMut(&mut Deck)) {
        for open in self.shown.iter_mut().chain(self.next.iter_mut()) {
            f(&mut open.deck);
        }
    }

    /// The sound heard: the clip coming in once a transition starts, else
    /// the one on screen.
    pub fn clip_audio(&self) -> Option<std::sync::Arc<crate::audio_mix::ClipAudioShared>> {
        let coming = self
            .next
            .as_ref()
            .filter(|_| self.transition.is_some())
            .and_then(|o| o.deck.clip_audio());
        coming.or_else(|| self.shown.as_ref().and_then(|o| o.deck.clip_audio()))
    }

    /// What the UI shows of it.
    pub fn snapshot(&self) -> PlaylistSnapshot {
        let on_screen = self.shown.as_ref().filter(|o| !o.gone);
        let remaining = on_screen.and_then(|open| {
            if open.clip {
                open.deck
                    .playback_snapshot()
                    .map(|p| (p.effective_out() - p.position_now()).max(0.0))
            } else {
                Some((self.settings.picture_secs - open.held).max(0.0))
            }
        });
        PlaylistSnapshot {
            items: self.items.clone(),
            settings: self.settings,
            playing: self.playing,
            shown: on_screen.map(|o| o.index),
            coming: match (&self.next, &self.opening) {
                (Some(next), _) if self.transition.is_some() || self.jump => Some(next.index),
                (None, Some(opening)) if self.jump => Some(opening.index),
                _ => None,
            },
            next_ready: self.next.as_ref().map(|o| o.index),
            failed: self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| self.failed.contains(&item.path))
                .map(|(i, _)| i)
                .collect(),
            remaining,
        }
    }
}

impl Drop for Playlist {
    fn drop(&mut self) {
        for open in self.shown.take().into_iter().chain(self.next.take()) {
            let_go(open.deck);
        }
    }
}

/// `open`'s place in a new list: where its item is now — the same place if
/// it is still there, else the first — or, taken out, the place of the item
/// after it.
fn reindex(open: &mut Open, items: &[PlaylistItem]) {
    if items.get(open.index).is_some_and(|i| i.path == open.path) {
        open.gone = false;
        return;
    }
    if let Some(index) = items.iter().position(|i| i.path == open.path) {
        open.index = index;
        open.gone = false;
    } else {
        open.index = open.index.min(items.len());
        open.gone = true;
    }
}

/// Open one item as a deck: a clip that holds its last frame (the playlist
/// moves on from there), or a picture.
fn open_item(
    context: &GpuContext,
    path: &str,
    size: (u32, u32),
    scaling_mode: ScalingMode,
) -> anyhow::Result<Deck> {
    let mut deck = match MediaKind::of(std::path::Path::new(path)) {
        Some(MediaKind::Image) => Deck::new_from_image(context, path, size.0, size.1)?,
        Some(MediaKind::Video | MediaKind::Audio) => {
            let deck = Deck::new_from_video(context, path, size.0, size.1)?;
            deck.video_set_loop_mode(crate::video::LoopMode::HoldLast);
            deck
        }
        _ => anyhow::bail!("not a clip, sound file or picture"),
    };
    deck.set_scaling_mode(scaling_mode);
    Ok(deck)
}

/// Close a deck on a thread: a clip's decoder is waited for as it stops.
fn let_go(deck: Box<Deck>) {
    let _ = std::thread::Builder::new()
        .name("playlist-close".into())
        .spawn(move || drop(deck));
}
