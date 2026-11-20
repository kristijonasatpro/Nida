//! The rundown: its commands, and taking a step.
//!
//! A step is taken whole or not at all. Everything it sets is checked first
//! — every bus and input, Main, the overlays, what it starts — and only then
//! is anything changed, all from one reading of the clock: the buses
//! together, Main, the overlays, the clips. A step with something missing is
//! refused and says what, rather than putting two thirds of a picture on
//! the wall.

use super::super::VardaApp;
use crate::bus::{BusInputRef, Together, TogetherRejected};
use crate::engine::value::render::OutputSource;
use crate::engine::value::timer::{TimerAction, TimerMode};
use crate::engine::{CommandResult, ErrorCode};
use crate::mixer::{MainSource, MixMode};
use crate::rundown::{OverlaySet, PlayAction, PlaySet, ScreenSet, Step, TimerSet};

/// What a clip or playlist is doing, for a wait on its end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlayState {
    /// Playing, and not at its end.
    pub running: bool,
    /// Played to its end, and staying there: a clip played once or holding
    /// its last frame, a playlist not looping at its last item.
    pub ended: bool,
    /// For a clip, where it is.
    pub clip: Option<ClipAt>,
}

/// A clip's playhead: how many times it has been sent somewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipAt {
    pub seeks: u64,
}

/// What a clip is doing, from its playhead. At its end is where it stands:
/// a clip played once stops at its out-point, one holding its last frame
/// stays there — not the end flag, set for one decoder tick, and left over
/// from before a seek while the clip is suspended.
pub(crate) fn clip_state(snap: &crate::video::PlaybackSnapshot) -> PlayState {
    let once = matches!(
        snap.loop_mode,
        crate::video::LoopMode::OneShot | crate::video::LoopMode::HoldLast
    );
    let half_frame = 0.5 / snap.frame_rate.max(1.0);
    // A clip whose length is not known yet is at no end.
    let out = snap.effective_out();
    let at_end = out > 0.0 && snap.position >= out - half_frame;
    PlayState {
        running: snap.playing && !at_end,
        ended: once && at_end,
        clip: Some(ClipAt { seeks: snap.seeks }),
    }
}

/// Whether what a wait awaits counts as seen playing now. See
/// [`VardaApp::watch_awaited_end`].
pub(crate) fn seen_playing(follow: &crate::rundown::Follow, state: &PlayState) -> bool {
    match state.clip {
        // Sent to its beginning by the take: once that seek has been dealt
        // with — its run since then, playing or played out already. A clip
        // not open at the take: once seen playing.
        Some(clip) if follow.restarted => match follow.seeks_before {
            Some(before) => clip.seeks != before,
            None => state.running,
        },
        // Carried on: its end, even one it is at already, is the one awaited.
        Some(_) => true,
        None => state.running,
    }
}

/// A clip or playlist a step is fading out: its sound goes down over the
/// step's fade, then it is stopped or paused — and its sound is kept down a
/// moment longer, so what its decoder had ready is not heard at full level.
#[derive(Debug, Clone)]
pub(crate) struct SoundFade {
    /// Its deck, by UUID.
    deck: String,
    /// The frame-clock reading it started at, and how long it takes.
    from: f64,
    secs: f32,
    /// Done at its end: stopped, back at the beginning, or paused.
    then: PlayAction,
    /// The level it starts from: full, or where an earlier fade of the same
    /// sound had got to.
    start: f32,
    /// When it was stopped or paused: the sound is let go a moment after.
    done_at: Option<f64>,
    /// A clip's Stop pressed by hand: its pause is a gesture an armed
    /// automation pass takes, when it lands at the fade's end.
    by_hand: bool,
}

impl SoundFade {
    /// Its level at `now`, 0 to 1.
    fn level_at(&self, now: f64) -> f32 {
        if self.done_at.is_some() {
            return 0.0;
        }
        let gone = ((now - self.from) / f64::from(self.secs)).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let left = (1.0 - gone) as f32;
        self.start * left
    }
}

/// How long a sound faded out is kept down after it is stopped or paused.
const QUIET_AFTER_SECS: f64 = 0.4;

/// The clips and playlists `step` starts or carries on till the next step.
fn till_next(step: &Step) -> Vec<String> {
    step.plays
        .iter()
        .filter(|play| play.until_next && matches!(play.action, PlayAction::Start | PlayAction::Resume))
        .map(|play| play.deck.clone())
        .collect()
}

/// What stands in the way of taking a step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepCheck {
    /// Why it cannot be taken at all, each a phrase: what it sets that is
    /// not there to be set.
    pub missing: Vec<String>,
    /// Why it cannot be faded to just now: what is still fading. A cut is
    /// not held up by these.
    pub fading: Vec<String>,
}

fn no_such_step(uuid: &str) -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::NotFound,
        message: format!("no rundown step with uuid {uuid}"),
    }
}

fn invalid(message: impl Into<String>) -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::InvalidInput,
        message: message.into(),
    }
}

/// On-air updates — one a frame drawn — a clip that a step paused or stopped
/// is kept from being started by Auto Play: long enough for the arrival the
/// step brings about to be seen (a fade to Main shows its first picture on
/// the third), and over before anything done by hand after it.
const HELD_FOR_UPDATES: u8 = 5;

/// How long a step that goes on by itself is tried for when its turn comes
/// while something is still fading, before it says it could not.
const FOLLOW_PATIENCE_SECS: f64 = 1.0;

fn unavailable(message: impl Into<String>) -> CommandResult {
    CommandResult::Err {
        code: ErrorCode::Unavailable,
        message: message.into(),
    }
}

/// What a deck is, to a step that starts and stops things.
enum Runs {
    Clip,
    Playlist,
    Timer,
}

impl VardaApp {
    /// Read-only access to the rundown.
    pub fn rundown(&self) -> &crate::rundown::Rundown {
        &self.rundown
    }

    /// Whether a channel is there and is an overlay, and its name.
    fn overlay_channel(&self, uuid: &str) -> Option<(bool, &str)> {
        let ch = self.mixer.channel(self.mixer.find_channel_by_uuid(uuid)?)?;
        Some((ch.overlay, ch.name.as_str()))
    }

    /// What a step can start and stop of a deck — or why it cannot, as a
    /// phrase naming the deck.
    fn runs(&self, deck_uuid: &str) -> Result<Runs, String> {
        let Ok((ch, dk)) = self.resolve_deck(deck_uuid) else {
            return Err("something it starts or stops".to_string());
        };
        let deck = &self.mixer.channels()[ch].decks[dk].deck;
        // A timer counts whatever the timeline does with its picture.
        if let Some(timer) = deck.timer() {
            return if timer.mode().has_transport() {
                Ok(Runs::Timer)
            } else {
                Err(format!("'{}' runs by the clock", deck.source_name()))
            };
        }
        let playlist = deck.playlist().is_some();
        if !playlist && deck.playback_snapshot().is_none() {
            return Err(format!("'{}' is not a clip, a playlist or a timer", deck.source_name()));
        }
        // A clip or a playlist on the timeline is the timeline's. A clip
        // following it, a seek or a pause here would be undone on the next
        // frame; and with a lane of its own, the arrangement decides when it
        // decodes at all: started outside its regions, it would be said to
        // play and not play.
        let chasing = deck
            .video_transport_sync()
            .is_some_and(|s| s.mode.is_chasing(self.transport.running()));
        let on_timeline = self
            .mixer
            .arrangement()
            .is_some_and(|arrangement| arrangement.lane(deck_uuid).is_some());
        if chasing || on_timeline {
            return Err(format!("'{}' follows the timeline", deck.source_name()));
        }
        Ok(if playlist { Runs::Playlist } else { Runs::Clip })
    }

    /// What a step does to a deck as `play` sets it: a timer it starts set
    /// to count down or up is one it starts, whatever the timer shows now.
    fn runs_for(&self, play: &PlaySet) -> Result<Runs, String> {
        if play.timer.is_some() && play.action == PlayAction::Start && self.is_timer(&play.deck) {
            return Ok(Runs::Timer);
        }
        self.runs(&play.deck)
    }

    /// Whether `deck` is a timer, whatever it shows.
    fn is_timer(&self, deck: &str) -> bool {
        self.resolve_deck(deck)
            .is_ok_and(|(ch, dk)| self.mixer.channels()[ch].decks[dk].deck.timer().is_some())
    }

    /// What stands in the way of taking `step` now. Nothing is changed.
    pub(crate) fn check_step(&self, step: &Step) -> StepCheck {
        self.check_step_among(step, &self.known_source_uuids())
    }

    /// [`Self::check_step`], `known` being the channels and decks there are
    /// — gathered once for a whole rundown.
    fn check_step_among(&self, step: &Step, known: &crate::bus::KnownSources) -> StepCheck {
        let mut check = StepCheck::default();
        // A show that did not open whole: nothing a step names can be
        // trusted to be what was meant (see `BusBank::quarantine_routing`).
        if self.production_buses.routing_quarantined() {
            check
                .missing
                .push("the show did not open whole, so nothing is routed".to_string());
            return check;
        }
        let sets = step.screen_sets();
        match self
            .production_buses
            .check_together(&sets, Together::Cut, known)
        {
            Err(TogetherRejected::Missing(what)) => check.missing.extend(what),
            Err(other) => check.missing.push(other.to_string()),
            Ok(()) => {
                let fade = Together::Fade(self.rundown.fade_secs());
                if let Err(TogetherRejected::Fading(buses)) =
                    self.production_buses.check_together(&sets, fade, known)
                {
                    check.fading.extend(buses);
                }
            }
        }
        let main = self.mixer.main();
        if (step.main.is_some() || !step.overlays.is_empty())
            && self.mixer.mix_mode() != MixMode::Program
        {
            check
                .missing
                .push("Main (the layered mix is on, not Main mode)".to_string());
        }
        if let Some(source) = &step.main {
            let overlay = match source {
                MainSource::Channel(uuid) => self.overlay_channel(uuid).filter(|(is, _)| *is),
                _ => None,
            };
            if !self.mixer.main_source_exists(source) {
                check.missing.push("Main's input".to_string());
            } else if let Some((_, name)) = overlay {
                // Taken to Main, an overlay is switched on instead: not what
                // the step was made to do.
                check
                    .missing
                    .push(format!("'{name}' for Main (it is an overlay now)"));
            } else if main.fade.is_some() && main.program != *source {
                check.fading.push("Main".to_string());
            }
        }
        for overlay in &step.overlays {
            match self.overlay_channel(&overlay.channel) {
                // Still fading the other way: turned round now, an overlay
                // on its way on would be gone before it was seen.
                Some((true, name)) => {
                    if main.overlay_settle_secs(&overlay.channel) > 0.0
                        && main.overlay_on(&overlay.channel) != overlay.on
                    {
                        check.fading.push(format!("'{name}'"));
                    }
                }
                Some((false, name)) => check
                    .missing
                    .push(format!("'{name}' as an overlay (it is not one now)")),
                None => check.missing.push("an overlay".to_string()),
            }
        }
        for play in &step.plays {
            if let Err(why) = self.runs_for(play) {
                check.missing.push(why);
            }
        }
        check
    }

    /// Whether a step's picture — the buses, Main and overlays it sets — is
    /// what is on now, or on its way. A step that sets none of them is not.
    fn step_shown(&self, step: &Step) -> bool {
        let sets = step.screen_sets();
        let main = self.mixer.main();
        step.sets_picture()
            && (sets.is_empty() || self.production_buses.shows_together(&sets))
            && step.main.as_ref().is_none_or(|source| main.program == *source)
            && step
                .overlays
                .iter()
                .all(|o| main.overlay_on(&o.channel) == o.on)
    }

    /// The rundown as the window and the API see it.
    pub(crate) fn rundown_snapshot(&self) -> crate::engine::RundownSnapshot {
        use crate::engine::{RundownSnapshot, RundownStepSnapshot, RundownWaitSnapshot};
        let rundown = &self.rundown;
        let known = self.known_source_uuids();
        let now = self.frame_stats.now_secs();
        RundownSnapshot {
            fade_secs: rundown.fade_secs(),
            steps: rundown
                .steps()
                .iter()
                .map(|step| {
                    let check = self.check_step_among(step, &known);
                    RundownStepSnapshot {
                        step: step.clone(),
                        missing: check.missing,
                        fading: check.fading,
                        shown: self.step_shown(step),
                    }
                })
                .collect(),
            last: rundown.last().map(str::to_string),
            next: rundown.next().map(|step| step.uuid.clone()),
            wait: rundown.follow().map(|follow| {
                let left = match &follow.until {
                    Some(deck) if follow.due.is_infinite() => self.left_to_play(deck).unwrap_or(0.0),
                    _ => (follow.due - now).max(0.0),
                };
                RundownWaitSnapshot {
                    from: follow.from.clone(),
                    to: follow.to.clone(),
                    secs_left: left as f32,
                    fade: follow.fade,
                    until: follow.until.clone(),
                }
            }),
            at_held: rundown.at_held().map(str::to_string),
            held_always: rundown.held_always().to_vec(),
        }
    }

    /// Take a step, all of it or none. Refused, it says why and nothing has
    /// changed.
    fn take_step(&mut self, uuid: &str, fade: bool) -> CommandResult {
        let Some(step) = self.rundown.step(uuid).cloned() else {
            return no_such_step(uuid);
        };
        let check = self.check_step(&step);
        let refused = |why: String| unavailable(format!("Step '{}' not taken — {why}", step.name));
        if !check.missing.is_empty() {
            return refused(format!("not in the show: {}", check.missing.join("; ")));
        }
        if fade && !check.fading.is_empty() {
            return refused(format!("still fading: {}", check.fading.join(", ")));
        }
        // What this step pauses or stops stays so as the step brings it on
        // air. What an earlier step in the same moment held back stays held:
        // a step that only starts a timer does not let it go. (A step that
        // starts the clip plays it itself; a hold only keeps Auto Play from
        // starting it, so it has nothing to undo.)
        // What the step being left played till the next step, this one does
        // not start or stop itself — stopped by this step, and held so as a
        // clip this step stops itself is — and what is on air before
        // anything moves, for the sound this step pauses or stops alone.
        let leaving = self.played_till_next(&step);
        let mut held = self.clips_held_by(&step);
        held.extend(leaving.iter().filter_map(|deck| self.held_entry(deck)));
        self.clips_held
            .retain(|(uuid, _, _)| !held.iter().any(|(again, _, _)| again == uuid));
        self.clips_held.extend(held);
        let on_air = self.on_air();
        // Read now, not at the top of the frame, and once: every bus starts
        // from this instant (see `cmd_bus_auto`).
        let now = self.frame_stats.now_secs();
        let how = if fade {
            Together::Fade(self.rundown.fade_secs())
        } else {
            Together::Cut
        };
        let known = self.known_source_uuids();
        let buses = match self.production_buses.take_together(
            &step.screen_sets(),
            how,
            now,
            &known,
        ) {
            Ok(buses) => buses,
            Err(why) => return refused(why.to_string()),
        };
        // How long until the picture has landed: the buses', and Main's fade
        // if the step takes Main — what is left of it, by the time it was
        // started with. A wait is not shorter (see `Rundown::taken`).
        let mut settle = buses.settle_secs as f32;
        if let Some(source) = &step.main {
            // Main's own take: its transition, its fade time, its notices.
            self.cmd_take_to_main(source.clone(), fade);
            if let Some(running) = &self.mixer.main().fade {
                settle = settle.max((1.0 - running.progress).max(0.0) * running.duration);
            }
        }
        for overlay in &step.overlays {
            let main = self.mixer.main_mut();
            main.set_overlay(&overlay.channel, overlay.on, fade);
            // What is left of its fade, if it is fading: one that was where
            // the step wants it already adds nothing.
            settle = settle.max(main.overlay_settle_secs(&overlay.channel));
        }
        let fade_secs = self.rundown.fade_secs();
        for deck in leaving {
            self.fade_out(&deck, now, fade.then_some(fade_secs), PlayAction::Stop);
        }
        // The clip it goes on at the end of, before it is sent to its
        // beginning: how many seeks it had had.
        let seeks_before = step
            .then_ends
            .as_deref()
            .and_then(|deck| self.video_playback_snapshot(deck))
            .map(|snap| snap.seeks);
        for play in &step.plays {
            // Sound alone — a clip or playlist on no screen and not on Main,
            // music — paused or stopped by a step faded to fades out with it,
            // on from wherever an earlier fade of it has got to.
            let alone = matches!(play.action, PlayAction::Pause | PlayAction::Stop)
                && on_air.anywhere(&play.deck) <= 0.0
                && matches!(self.runs(&play.deck), Ok(Runs::Clip | Runs::Playlist));
            if alone {
                self.fade_out(&play.deck, now, fade.then_some(fade_secs), play.action);
            } else {
                // Its own now: a fade an earlier step began on it is over.
                self.rundown_fades.retain(|f| f.deck != play.deck);
                self.run_play(play);
            }
        }
        // What it plays till the next step, kept should it be removed
        // before then.
        self.rundown_till_next = till_next(&step);
        self.rundown.taken(uuid, fade, now, settle);
        if let Some(seeks) = seeks_before {
            self.rundown.set_seeks_before(seeks);
        }
        log::info!(
            "Rundown: '{}' taken with a {}",
            step.name,
            if fade { "fade" } else { "cut" }
        );
        CommandResult::Ok
    }

    /// The clips `step` pauses or stops, each with the decoder it has now,
    /// to be held for the next few on-air updates.
    ///
    /// A step that puts a clip on a screen and says it is paused, or
    /// stopped, means a still picture there: Auto Play, which starts a clip
    /// as it goes on air, gives way to the step for the arrival the step
    /// brings about (see `update_on_air`). That arrival is seen within a
    /// frame or three of the take — a cut on the next update, a fade when
    /// its first picture is drawn — so the hold lasts [`HELD_FOR_UPDATES`]
    /// and no longer: it is not worked out which of the step's routes, taps
    /// and layers will bring the clip on, and a clip the step brings on
    /// nowhere is free again before a hand can put it on air. For those few
    /// frames a clip is still whichever step, or hand, puts it on air: a
    /// step that starts it is what starts it.
    fn clips_held_by(&self, step: &Step) -> Vec<(String, usize, u8)> {
        step.plays
            .iter()
            .filter(|play| matches!(play.action, PlayAction::Pause | PlayAction::Stop))
            .filter_map(|play| {
                let (ch, dk) = self.resolve_deck(&play.deck).ok()?;
                let identity = self.mixer.channels()[ch].decks[dk].deck.playback_identity()?;
                Some((play.deck.clone(), identity, HELD_FOR_UPDATES))
            })
            .collect()
    }

    /// The clips and playlists the step taken last played till the next
    /// step — started or carried on, with `until_next` — that `step` does
    /// not start or stop itself: to be faded out and stopped as it is taken.
    fn played_till_next(&self, step: &Step) -> Vec<String> {
        let decks = match self.rundown.last().and_then(|last| self.rundown.step(last)) {
            Some(left) => till_next(left),
            // The step taken last removed since: what it played till the next
            // step when it was taken still goes.
            None => self.rundown_till_next.clone(),
        };
        decks
            .into_iter()
            .filter(|deck| !step.plays.iter().any(|own| &own.deck == deck))
            .filter(|deck| matches!(self.runs(deck), Ok(Runs::Clip | Runs::Playlist)))
            .collect()
    }

    /// The clip or playlist `deck` held from Auto Play for the next few
    /// on-air updates, as a step that stops it holds it.
    fn held_entry(&self, deck: &str) -> Option<(String, usize, u8)> {
        let (ch, dk) = self.resolve_deck(deck).ok()?;
        let identity = self.mixer.channels()[ch].decks[dk].deck.playback_identity()?;
        Some((deck.to_string(), identity, HELD_FOR_UPDATES))
    }

    /// Stop a clip or playlist as its Stop button does: its sound faded out
    /// — over `secs`, else the Stop fade set — then paused. One already
    /// fading out is stopped at once: a second Stop is not a slower one. One
    /// the timeline plays (it has a lane, or chases the transport) is paused
    /// at once, as its pause always did, the timeline deciding after. One not
    /// playing is left as it is.
    pub fn cmd_stop_with_fade(&mut self, deck_uuid: &str, secs: Option<f32>) -> CommandResult {
        let Ok((ch, dk)) = self.resolve_deck(deck_uuid) else {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("No input {deck_uuid}"),
            };
        };
        if let Some(secs) = secs {
            if !secs.is_finite() || !(0.0..=crate::audio_mix::MAX_STOP_FADE_SECS).contains(&secs) {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: format!(
                        "A stop fades for 0 to {} seconds",
                        crate::audio_mix::MAX_STOP_FADE_SECS
                    ),
                };
            }
        }
        let deck = &self.mixer.channels()[ch].decks[dk].deck;
        let playlist = deck.playlist().map(crate::deck::playlist::Playlist::is_playing);
        let clip = deck.playback_snapshot().map(|snap| snap.playing);
        let Some(playing) = playlist.or(clip) else {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: format!("'{}' has no sound to stop", deck.source_name()),
            };
        };
        let is_playlist = playlist.is_some();
        if !playing {
            // Not playing as far as can be seen — a Play sent a moment ago
            // may not have reached the decoder yet: paused all the same. Its
            // pause noted only where there is a Play to undo — a curve, or a
            // take this pass — so a Stop on an idle clip leaves no curve to
            // hold it paused later.
            let key = format!("deck_{deck_uuid}:{}", crate::video::modulation::PLAY);
            let pending =
                self.mixer.modulation().has_modulation(&key) || self.recording_take(&key);
            self.pause_now(deck_uuid, is_playlist, pending);
            return CommandResult::Ok;
        }
        if !matches!(self.runs(deck_uuid), Ok(Runs::Clip | Runs::Playlist)) {
            // The timeline's: paused at once, as its pause always did.
            self.pause_now(deck_uuid, is_playlist, true);
            return CommandResult::Ok;
        }
        let fading = self
            .rundown_fades
            .iter()
            .any(|fade| fade.deck == deck_uuid && fade.done_at.is_none());
        let secs = (!fading)
            .then(|| secs.unwrap_or_else(|| self.audio_mix.stop_fade_secs()))
            .filter(|secs| *secs > 0.0);
        let now = self.frame_stats.now_secs();
        self.fade_out(deck_uuid, now, secs, PlayAction::Pause);
        match self
            .rundown_fades
            .iter_mut()
            .find(|fade| fade.deck == deck_uuid && fade.done_at.is_none())
        {
            // Its pause noted for an armed pass when it lands, not now: a
            // value written now would pause the clip at once.
            Some(fade) => fade.by_hand = true,
            None if !is_playlist => {
                self.note_live_video_write(deck_uuid, crate::video::modulation::PLAY, 0.0);
            }
            None => {}
        }
        CommandResult::Ok
    }

    /// A clip's Stop by hand landed: its pause noted for an armed automation
    /// pass, as its own pause button's is. A playlist has no such curve.
    fn note_hand_stop(&mut self, deck: &str) {
        let clip = self.resolve_deck(deck).is_ok_and(|(ch, dk)| {
            self.mixer.channels()[ch].decks[dk].deck.playlist().is_none()
        });
        if clip {
            self.note_live_video_write(deck, crate::video::modulation::PLAY, 0.0);
        }
    }

    /// Pause a clip or playlist now — told to pause, not toggled, so a Stop
    /// can never start it. A clip's pause by hand (`gesture`) is noted for an
    /// armed automation pass, as its own pause button's is.
    fn pause_now(&mut self, deck_uuid: &str, playlist: bool, gesture: bool) {
        if playlist {
            let _ = self.edit_playlist(deck_uuid, |list| list.set_playing(false));
        } else {
            self.exec_on_deck(deck_uuid, |d| d.video_set_playing(false));
            if gesture {
                self.note_live_video_write(deck_uuid, crate::video::modulation::PLAY, 0.0);
            }
        }
    }

    /// The clips and playlists whose sound is fading out to a stop now, by
    /// deck UUID: a Stop pressed, or the rundown's fade.
    pub(crate) fn sounds_stopping(&self) -> Vec<String> {
        self.rundown_fades
            .iter()
            .filter(|fade| fade.done_at.is_none())
            .map(|fade| fade.deck.clone())
            .collect()
    }

    /// Fade a clip or playlist's sound out over `secs` from `now`, then do
    /// `then` to it — at once with no fade.
    fn fade_out(&mut self, deck: &str, now: f64, secs: Option<f32>, then: PlayAction) {
        // On from where an earlier fade of it has got to — never back up.
        let start = self
            .rundown_fades
            .iter()
            .find(|f| f.deck == deck)
            .map_or(1.0, |f| f.level_at(now));
        // A Stop by hand still to land keeps its gesture through the fade
        // that takes over from it.
        let by_hand = self
            .rundown_fades
            .iter()
            .any(|f| f.deck == deck && f.done_at.is_none() && f.by_hand);
        self.rundown_fades.retain(|f| f.deck != deck);
        match secs.filter(|secs| *secs > 0.0) {
            Some(secs) if start > 0.0 => self.rundown_fades.push(SoundFade {
                deck: deck.to_string(),
                from: now,
                secs,
                then,
                start,
                done_at: None,
                by_hand,
            }),
            _ => {
                self.run_play(&PlaySet::new(deck, then));
                if by_hand {
                    self.note_hand_stop(deck);
                }
                // Down already: kept down a moment, as at a fade's end.
                if start < 1.0 {
                    self.rundown_fades.push(SoundFade {
                        deck: deck.to_string(),
                        from: now,
                        secs: 1.0,
                        then,
                        start: 0.0,
                        done_at: Some(now),
                        by_hand: false,
                    });
                }
            }
        }
    }

    /// The fades and the times of day of the show before: let go, so none
    /// stops or turns down a deck of the show opening.
    pub(crate) fn forget_rundown_live(&mut self) {
        // What was fading out is stopped (or paused) now, as its fade would
        // have — not back up to full while the next show loads, nor playing
        // on should it not load.
        let pending: Vec<PlaySet> = self
            .rundown_fades
            .iter()
            .filter(|fade| fade.done_at.is_none())
            .map(|fade| PlaySet::new(fade.deck.clone(), fade.then))
            .collect();
        for play in &pending {
            self.run_play(play);
        }
        // Kept down a moment, as at a fade's end — what the decoders had
        // ready not heard at full level — then let go by run_sound_fades;
        // stopped already, they stop nothing of the show opening.
        let now = self.frame_stats.now_secs();
        for fade in &mut self.rundown_fades {
            if fade.done_at.is_none() {
                fade.done_at = Some(now);
            }
        }
        self.rundown_till_next.clear();
        self.rundown_day_was = None;
        self.run_sound_fades(now);
    }

    /// A show's scene put in place: the fades kept down for the show before
    /// are let go — its decks are gone, and the new ones, under the same
    /// UUIDs it may be, are not to be turned down.
    pub(crate) fn rundown_scene_replaced(&mut self) {
        // The old show's sound sources retired — or bound to the new decks —
        // first: their gates opened before, what they had buffered could be
        // heard at full level.
        self.update_audio_mix();
        self.rundown_fades.clear();
        self.audio_mix.set_step_fades(std::collections::HashMap::new());
    }

    /// Once a frame: the sounds being faded out turned down as far as their
    /// fade has gone, stopped or paused at its end, and let go a moment
    /// after.
    fn run_sound_fades(&mut self, now: f64) {
        // A deck deleted meanwhile is let go: its fade must not stop or
        // turn down whatever comes back under its UUID.
        let gone: Vec<String> = self
            .rundown_fades
            .iter()
            .filter(|fade| self.resolve_deck(&fade.deck).is_err())
            .map(|fade| fade.deck.clone())
            .collect();
        self.rundown_fades.retain(|fade| !gone.contains(&fade.deck));
        let mut levels = std::collections::HashMap::new();
        let mut done = Vec::new();
        let mut by_hand = Vec::new();
        self.rundown_fades.retain_mut(|fade| {
            if let Some(at) = fade.done_at {
                if now - at >= QUIET_AFTER_SECS {
                    return false;
                }
                levels.insert(fade.deck.clone(), 0.0);
                return true;
            }
            let gone = (now - fade.from) / f64::from(fade.secs);
            if gone >= 1.0 {
                done.push(PlaySet::new(fade.deck.clone(), fade.then));
                if fade.by_hand {
                    by_hand.push(fade.deck.clone());
                }
                fade.done_at = Some(now);
                levels.insert(fade.deck.clone(), 0.0);
            } else {
                levels.insert(fade.deck.clone(), fade.level_at(now));
            }
            true
        });
        for play in &done {
            self.run_play(play);
        }
        // A clip's Stop by hand lands now: its pause noted for an armed pass.
        for deck in by_hand {
            self.note_hand_stop(&deck);
        }
        self.audio_mix.set_step_fades(levels);
    }

    /// Start, carry on, pause or stop a clip, a playlist or a timer — a
    /// timer started set as the step says. One that cannot be —
    /// [`Self::check_step`] has said so — is passed over.
    fn run_play(&mut self, play: &PlaySet) {
        let (deck_uuid, action) = (play.deck.as_str(), play.action);
        let Ok(runs) = self.runs_for(play) else {
            return;
        };
        match runs {
            Runs::Clip => {
                let rewind = matches!(action, PlayAction::Start | PlayAction::Stop);
                let playing = matches!(action, PlayAction::Start | PlayAction::Resume);
                if rewind {
                    if let Some(snap) = self.video_playback_snapshot(deck_uuid) {
                        self.exec_on_deck(deck_uuid, |d| d.video_seek(snap.in_point));
                    }
                }
                self.exec_on_deck(deck_uuid, |d| d.video_set_playing(playing));
            }
            Runs::Playlist => {
                let _ = self.edit_playlist(deck_uuid, |list| match action {
                    PlayAction::Start => {
                        list.show_item(0);
                        list.set_playing(true);
                    }
                    PlayAction::Resume => list.set_playing(true),
                    PlayAction::Pause => list.set_playing(false),
                    PlayAction::Stop => {
                        list.set_playing(false);
                        list.show_item(0);
                    }
                });
            }
            Runs::Timer => {
                let set = play.timer;
                let _ = self.edit_timer(deck_uuid, |timer| match action {
                    PlayAction::Start => {
                        // Counting down from this talk's length, or up.
                        if let Some(set) = set {
                            let mut settings = timer.settings().clone();
                            match set {
                                TimerSet::Down { secs } => {
                                    settings.mode = TimerMode::Countdown;
                                    settings.duration_secs = secs;
                                }
                                TimerSet::Up => settings.mode = TimerMode::CountUp,
                            }
                            // For this run: an undo, or the show saved, keeps
                            // the timer as it is set.
                            timer.run_as(settings);
                        }
                        timer.control(TimerAction::Reset);
                        timer.control(TimerAction::Start);
                    }
                    PlayAction::Resume => timer.control(TimerAction::Start),
                    PlayAction::Pause => timer.control(TimerAction::Pause),
                    PlayAction::Stop => timer.control(TimerAction::Reset),
                });
            }
        }
    }

    /// Tell the operator a step was not taken: a press that did nothing
    /// must say so on the screen, wherever it came from.
    fn tell_not_taken(&mut self, result: &CommandResult) {
        if let CommandResult::Err { message, .. } = result {
            log::warn!("Rundown: {message}");
            self.session.notifications.warn(message.clone());
        }
    }

    /// Take a step by hand. The operator has stepped in, so a wait being
    /// counted out is over, taken or not.
    pub fn cmd_take_rundown_step(&mut self, uuid: &str, fade: bool) -> CommandResult {
        self.rundown.drop_wait();
        let result = self.take_step(uuid, fade);
        self.tell_not_taken(&result);
        result
    }

    /// Take the next step. With `expect`, only if that is the step: a press
    /// meant for one step, arriving after the rundown went on to it by
    /// itself, would otherwise take the one after.
    pub fn cmd_rundown_go(&mut self, fade: bool, expect: Option<&str>) -> CommandResult {
        let Some(next) = self.rundown.next() else {
            let result = unavailable(if self.rundown.steps().is_empty() {
                "The rundown has no steps"
            } else {
                "The rundown is at its end"
            });
            self.tell_not_taken(&result);
            return result;
        };
        if expect.is_some_and(|expected| expected != next.uuid) {
            // Not the operator stepping in: the press was for a step that
            // has been taken already, and what runs now runs on.
            let result = unavailable(format!(
                "The rundown has moved on — '{}' is next now",
                next.name
            ));
            self.tell_not_taken(&result);
            return result;
        }
        let next = next.uuid.clone();
        self.cmd_take_rundown_step(&next, fade)
    }

    /// Once a frame, after this frame's commands and at the instant the
    /// buses were moved on to: a wait that has run out takes the step it
    /// leads to, as its own step was taken.
    ///
    /// A step whose only hindrance is a fade still landing is tried again on
    /// the frames that follow, for [`FOLLOW_PATIENCE_SECS`]: Main's fade is
    /// counted in frames drawn and ends a frame or two after the wait made as
    /// long as it. Refused for any other reason, or when the patience is up,
    /// it says so, the wait is over, and it is not tried again.
    pub(crate) fn advance_rundown(&mut self, now: f64) {
        self.advance_rundown_by(now, crate::deck::timer::time_of_day());
    }

    /// [`Self::advance_rundown`], the time of day being `day` — seconds
    /// after local midnight, read with `now`: the sounds being faded out
    /// moved on, and a step whose time of day the clock has come to, while
    /// `GO` would take it next, taken as a wait run out is.
    pub(crate) fn advance_rundown_by(&mut self, now: f64, day: f64) {
        self.run_sound_fades(now);
        // The time read last, with the step next once that frame was done: a
        // step made next since — by hand — has its time counted from now on.
        let next = self.rundown.next().map(|step| step.uuid.clone());
        if let Some((was, next_then)) = self.rundown_day_was.take() {
            let due = self
                .rundown
                .due_at(was, day)
                .filter(|_| next_then == next)
                .map(|step| (step.uuid.clone(), step.name.clone(), false))
                .or_else(|| {
                    self.rundown
                        .due_always(was, day)
                        .map(|step| (step.uuid.clone(), step.name.clone(), true))
                });
            if let Some((uuid, name, always)) = due {
                if always {
                    log::info!("Rundown: '{name}' is due at its time of day, wherever the rundown is");
                } else {
                    log::info!("Rundown: '{name}' is due at its time of day");
                }
                self.rundown.start_at(&uuid, now);
            }
        }
        self.watch_awaited_end(now);
        self.run_wait(now);
        // Kept with the step next now — after what this frame took — so a
        // step this frame made next goes by its time from here.
        self.rundown_day_was = Some((day, self.rundown.next().map(|step| step.uuid.clone())));
    }

    /// A wait for a clip or playlist's end: seen playing, then seen at its
    /// end, it is due. Gone from the show, the wait is dropped.
    ///
    /// A clip the step started from its beginning is seen playing only once
    /// the seek there has happened (its count of seeks moved on since just
    /// before the take) — the seek is not at once, and the run before may
    /// still say it is near its end. A clip carried on counts at once: its
    /// end, even one it is at already, is the one awaited. A playlist is
    /// seen playing when it plays and is not at its end: carried on from
    /// its end, it starts again from its first item.
    fn watch_awaited_end(&mut self, now: f64) {
        let Some(follow) = self.rundown.awaited().cloned() else {
            return;
        };
        let Some(deck) = follow.until.as_deref() else {
            return;
        };
        let Some(state) = self.play_state(deck) else {
            self.rundown.drop_wait();
            return;
        };
        if !follow.seen_running && seen_playing(&follow, &state) {
            self.rundown.saw_running();
        }
        if state.ended && self.rundown.end_came(now) {
            log::info!("Rundown: what the step waited for has ended — on to the next");
        }
    }

    /// Whether the clip or playlist `deck` is playing, has played to its end
    /// — a clip set to play once or hold its last frame, a list that does
    /// not loop — or neither; `None` when it is not a clip or playlist in
    /// the show.
    pub(crate) fn play_state(&self, deck: &str) -> Option<PlayState> {
        let (ch, dk) = self.resolve_deck(deck).ok()?;
        let deck = &self.mixer.channels()[ch].decks[dk].deck;
        if let Some(list) = deck.playlist() {
            let ended = list.is_finished();
            return Some(PlayState {
                running: list.is_playing() && !ended,
                ended,
                clip: None,
            });
        }
        deck.playback_snapshot().map(|snap| clip_state(&snap))
    }

    /// About how long the clip or playlist `deck` has left to play, if
    /// known: what a wait for its end says.
    fn left_to_play(&self, deck: &str) -> Option<f64> {
        let (ch, dk) = self.resolve_deck(deck).ok()?;
        let deck = &self.mixer.channels()[ch].decks[dk].deck;
        // A playlist's time left is its shown item's: the whole list's only
        // on its last.
        if let Some(list) = deck.playlist() {
            let snap = list.snapshot();
            let last = snap.shown.is_some_and(|i| i + 1 >= snap.items.len());
            return snap.remaining.filter(|_| last);
        }
        let snap = deck.playback_snapshot()?;
        let speed = snap.effective_speed.abs();
        (speed > 0.0).then(|| ((snap.effective_out() - snap.position_now()) / speed).max(0.0))
    }

    /// A wait run out — a step going on by itself, or one due at its time of
    /// day — takes the step it leads to.
    fn run_wait(&mut self, now: f64) {
        let Some(follow) = self.rundown.due(now) else {
            return;
        };
        let (to, fade, overdue) = (follow.to.clone(), follow.fade, now - follow.due);
        let landing = self
            .rundown
            .step(&to)
            .map(|step| self.check_step(step))
            .is_some_and(|check| check.missing.is_empty() && !check.fading.is_empty());
        if fade && landing && overdue < FOLLOW_PATIENCE_SECS {
            return;
        }
        self.rundown.drop_wait();
        let result = self.take_step(&to, fade);
        self.tell_not_taken(&result);
    }

    /// What is on now, as a step sets it: what each of `buses` shows — every
    /// bus but one the stream or the recording takes, when none are named —
    /// and, with `main`, what is on Main and every overlay on or off.
    fn step_of_now(&self, name: &str, buses: Option<&[String]>, main: bool) -> Step {
        let mut step = Step::new(name);
        let on_air = [
            &self.stream_settings().options.source,
            &self.record_settings().options.source,
        ];
        let wanted = |uuid: &str| match buses {
            Some(named) => named.iter().any(|b| b == uuid),
            // Left out unless asked for: a step is for the screens, and one
            // made for the wall must not change what the stream sends.
            None => !on_air
                .iter()
                .any(|source| matches!(source, OutputSource::BusProgram(b) if b == uuid)),
        };
        step.screens = self
            .production_buses
            .iter()
            .filter(|bus| wanted(bus.uuid()))
            .filter_map(|bus| self.screen_of_now(bus.uuid()))
            .collect();
        if main {
            let on_main = self.mixer.main();
            step.main = Some(on_main.program.clone());
            step.overlays = self
                .mixer
                .channels()
                .iter()
                .filter(|ch| ch.overlay)
                .map(|ch| OverlaySet {
                    channel: ch.uuid().to_string(),
                    on: on_main.overlay_on(ch.uuid()),
                })
                .collect();
        }
        step
    }

    /// What a bus shows now — where a take is heading, if one is running —
    /// as a step sets it; none for a bus that is gone. An input that is not
    /// in the show is kept as it is: the step then says so, where leaving
    /// the bus out would lose a screen the operator ticked without a word.
    fn screen_of_now(&self, bus: &str) -> Option<ScreenSet> {
        let (input, _) = self.production_buses.get(bus)?.heading();
        Some(ScreenSet {
            bus: bus.to_string(),
            input: input.clone(),
        })
    }

    /// Add a step keeping what is on now (see [`Self::step_of_now`]).
    pub fn cmd_add_rundown_step(
        &mut self,
        name: &str,
        buses: Option<&[String]>,
        main: bool,
    ) -> CommandResult {
        let name = name.trim();
        if name.is_empty() {
            return invalid("a step needs a name");
        }
        if let Some(gone) = buses
            .unwrap_or_default()
            .iter()
            .find(|uuid| !self.production_buses.contains(uuid))
        {
            return CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("no bus with uuid {gone}"),
            };
        }
        let step = self.step_of_now(name, buses, main);
        let uuid = self.rundown.add(step);
        log::info!("Rundown: added step '{name}' uuid={uuid}");
        CommandResult::OkWithId { uuid }
    }

    /// Put a copy of a step right after it.
    pub fn cmd_duplicate_rundown_step(&mut self, uuid: &str) -> CommandResult {
        match self.rundown.duplicate(uuid) {
            Some(uuid) => CommandResult::OkWithId { uuid },
            None => no_such_step(uuid),
        }
    }

    pub fn cmd_remove_rundown_step(&mut self, uuid: &str) -> CommandResult {
        match self.rundown.remove(uuid) {
            Some(step) => {
                log::info!("Rundown: removed step '{}' uuid={uuid}", step.name);
                CommandResult::Ok
            }
            None => no_such_step(uuid),
        }
    }

    pub fn cmd_rename_rundown_step(&mut self, uuid: &str, name: &str) -> CommandResult {
        let name = name.trim();
        if name.is_empty() {
            return invalid("a step needs a name");
        }
        self.edited(uuid, |rundown| rundown.rename(uuid, name))
    }

    /// Move a step one place later, or earlier. At the end of the list
    /// already, it stays, and that is not an error.
    pub fn cmd_move_rundown_step(&mut self, uuid: &str, later: bool) -> CommandResult {
        if self.rundown.step(uuid).is_none() {
            return no_such_step(uuid);
        }
        self.rundown.move_step(uuid, later);
        CommandResult::Ok
    }

    /// Do an edit that only a missing step fails.
    fn edited(
        &mut self,
        uuid: &str,
        edit: impl FnOnce(&mut crate::rundown::Rundown) -> bool,
    ) -> CommandResult {
        if edit(&mut self.rundown) {
            CommandResult::Ok
        } else {
            no_such_step(uuid)
        }
    }

    /// What a bus shows in a step, or with `None` leave it as it is. The
    /// bus and the input must be there now: a step is not made against
    /// what does not exist, to fail only when it is taken.
    pub fn cmd_set_rundown_screen(
        &mut self,
        uuid: &str,
        bus: &str,
        input: Option<BusInputRef>,
    ) -> CommandResult {
        if let Some(input) = &input {
            if !self.production_buses.contains(bus) {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("no bus with uuid {bus}"),
                };
            }
            if let Err(unknown) = self.resolve_bus_input(input) {
                return unknown.into();
            }
        }
        self.edited(uuid, |rundown| rundown.set_screen(uuid, bus, input))
    }

    /// What a step takes to Main, or with `None` leave Main as it is.
    pub fn cmd_set_rundown_main(&mut self, uuid: &str, main: Option<MainSource>) -> CommandResult {
        if let Some(source) = &main {
            if !self.mixer.main_source_exists(source) {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("nothing in the show with uuid {}", source.uuid().unwrap_or_default()),
                };
            }
            if let MainSource::Channel(channel) = source {
                if self.overlay_channel(channel).is_some_and(|(is, _)| is) {
                    return invalid(
                        "That channel is an overlay — a step switches it on or off instead",
                    );
                }
            }
        }
        self.edited(uuid, |rundown| rundown.set_main(uuid, main))
    }

    /// Whether a step switches an overlay channel on or off, or with `None`
    /// leaves it as it is.
    pub fn cmd_set_rundown_overlay(
        &mut self,
        uuid: &str,
        channel: &str,
        on: Option<bool>,
    ) -> CommandResult {
        if on.is_some() {
            match self.overlay_channel(channel) {
                Some((true, _)) => {}
                Some((false, _)) => {
                    return invalid("That channel is not an overlay — a step takes it to Main instead")
                }
                None => {
                    return CommandResult::Err {
                        code: ErrorCode::NotFound,
                        message: format!("no channel with uuid {channel}"),
                    }
                }
            }
        }
        self.edited(uuid, |rundown| rundown.set_overlay(uuid, channel, on))
    }

    /// What a step does to a clip, a playlist or a timer, or with `None`
    /// nothing.
    pub fn cmd_set_rundown_play(
        &mut self,
        uuid: &str,
        deck: &str,
        action: Option<PlayAction>,
    ) -> CommandResult {
        if action.is_some() {
            if self.resolve_deck(deck).is_err() {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("no deck with uuid {deck}"),
                };
            }
            if let Err(why) = self.runs(deck) {
                return invalid(format!("A step cannot start or stop that: {why}"));
            }
        }
        self.edited(uuid, |rundown| rundown.set_play(uuid, deck, action))
    }

    /// Whether a clip or playlist a step starts or carries on is faded out
    /// and stopped when another step is taken.
    pub fn cmd_set_rundown_until_next(&mut self, uuid: &str, deck: &str, on: bool) -> CommandResult {
        if self.rundown.step(uuid).is_none() {
            return no_such_step(uuid);
        }
        if on && !matches!(self.runs(deck), Ok(Runs::Clip | Runs::Playlist)) {
            return invalid("Only a clip or a playlist is faded out at the next step");
        }
        if self.rundown.set_until_next(uuid, deck, on) {
            CommandResult::Ok
        } else {
            invalid("The step does not start or stop that")
        }
    }

    /// How a step sets a timer it starts: down from a length of its own, or
    /// up — or with `None` as the timer is set.
    pub fn cmd_set_rundown_timer(
        &mut self,
        uuid: &str,
        deck: &str,
        timer: Option<TimerSet>,
    ) -> CommandResult {
        if self.rundown.step(uuid).is_none() {
            return no_such_step(uuid);
        }
        if timer.is_some() && !self.is_timer(deck) {
            return invalid("Only a timer is set so");
        }
        if matches!(timer, Some(TimerSet::Down { secs: 0 })) {
            return invalid("A count down is longer than no time");
        }
        if self.rundown.set_timer(uuid, deck, timer) {
            CommandResult::Ok
        } else {
            invalid("The step does not start or stop that timer")
        }
    }

    /// The time of day a step is taken by itself at, seconds after
    /// midnight, or with `None` not; `always`, if given, whether even when
    /// not next.
    pub fn cmd_set_rundown_at(
        &mut self,
        uuid: &str,
        at_secs: Option<u32>,
        always: Option<bool>,
    ) -> CommandResult {
        if at_secs.is_some_and(|secs| secs >= crate::rundown::DAY_SECS) {
            return invalid("A time of day is before midnight");
        }
        self.edited(uuid, |rundown| rundown.set_at(uuid, at_secs, always))
    }

    /// How long after a step the next is taken by itself, or with `None`
    /// not until the operator says.
    pub fn cmd_set_rundown_then(
        &mut self,
        uuid: &str,
        secs: Option<f32>,
        when_ends: Option<&str>,
    ) -> CommandResult {
        if let Some(deck) = when_ends {
            if self.rundown.step(uuid).is_some_and(|step| !step.plays_on(deck)) {
                return invalid("The step does not start or carry on that clip or playlist");
            }
            if !matches!(self.runs(deck), Ok(Runs::Clip | Runs::Playlist)) {
                return invalid("Only a clip or a playlist ends: a timer or anything else does not");
            }
            return self.edited(uuid, |rundown| rundown.set_then_ends(uuid, Some(deck)));
        }
        if secs.is_some_and(|secs| crate::rundown::valid_then(secs).is_none()) {
            return invalid("the wait must be more than no seconds");
        }
        self.edited(uuid, |rundown| rundown.set_then(uuid, secs))
    }

    /// Make a step's picture what is on now, for the parts it sets: the
    /// buses it lists, and Main and the overlays if it sets them. What it
    /// starts and its wait stay.
    pub fn cmd_capture_rundown_step(&mut self, uuid: &str) -> CommandResult {
        let Some(step) = self.rundown.step(uuid) else {
            return no_such_step(uuid);
        };
        let on_main = self.mixer.main();
        let now = Step {
            // A bus that is gone keeps what the step had, to be taken out.
            screens: step
                .screens
                .iter()
                .map(|old| self.screen_of_now(&old.bus).unwrap_or_else(|| old.clone()))
                .collect(),
            main: step.main.as_ref().map(|_| on_main.program.clone()),
            overlays: step
                .overlays
                .iter()
                .map(|old| OverlaySet {
                    channel: old.channel.clone(),
                    on: on_main.overlay_on(&old.channel),
                })
                .collect(),
            ..Step::new(step.name.as_str())
        };
        self.edited(uuid, |rundown| rundown.set_picture(uuid, now))
    }

    /// Make this the step `GO` takes next, without taking anything.
    pub fn cmd_set_rundown_next(&mut self, uuid: &str) -> CommandResult {
        self.edited(uuid, |rundown| rundown.set_next(uuid))
    }

    /// Drop the wait of a step that goes on by itself, and hold the time of
    /// day of the step next.
    pub fn cmd_rundown_hold(&mut self) -> CommandResult {
        if self.rundown.hold(crate::deck::timer::time_of_day()) {
            log::info!("Rundown: held — it goes on when the operator says");
        }
        CommandResult::Ok
    }

    /// Seconds the buses take to fade to a step.
    pub fn cmd_set_rundown_fade(&mut self, secs: f32) -> CommandResult {
        if self.rundown.set_fade_secs(secs) {
            CommandResult::Ok
        } else {
            invalid(format!("the fade time must be finite and positive, got {secs}"))
        }
    }

    /// A bus was removed: steps no longer set it.
    pub(crate) fn rundown_forget_bus(&mut self, bus: &str) {
        self.rundown.forget_bus(bus);
    }

    /// A channel or deck was deleted for good: steps let go of it, and the
    /// operator is told which — a step that has quietly stopped setting a
    /// screen is found out on air otherwise.
    pub(crate) fn rundown_forget_source(&mut self, uuid: &str) {
        // Its fade let go at once, not on the next frame: a deck put back by
        // an undo in the same breath is another decoder, not to be faded.
        self.rundown_fades.retain(|fade| fade.deck != uuid);
        self.rundown_till_next.retain(|deck| deck != uuid);
        let steps = self.rundown.forget_source(uuid);
        if !steps.is_empty() {
            let text = format!(
                "Rundown: {} no longer use{} what was deleted",
                steps.join(", "),
                if steps.len() == 1 { "s" } else { "" }
            );
            log::info!("{text}");
            self.session.notifications.info(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{seen_playing, ClipAt, PlayState};
    use crate::bus::BusInputRef;
    use crate::engine::{CommandResult, EngineCommand};
    use crate::rundown::PlayAction;

    type App = super::super::super::VardaApp;

    fn headless_app() -> Option<App> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        App::new(gpu, &crate::testing::headless_config()).ok()
    }

    fn id(result: CommandResult) -> String {
        match result {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected an id, got {other:?}"),
        }
    }

    fn notices(app: &App) -> Vec<String> {
        app.session
            .notifications
            .visible()
            .iter()
            .map(|n| n.message.clone())
            .collect()
    }

    /// The show's first bus and first channel, and a step that sets nothing.
    fn step(app: &mut App) -> (String, String, String) {
        let state = app.build_engine_state();
        let step = id(app.execute_command(EngineCommand::AddRundownStep {
            name: "B".into(),
            buses: Some(vec![]),
            main: false,
        }));
        (
            state.buses[0].uuid.clone(),
            state.mixer.channels[0].uuid.clone(),
            step,
        )
    }

    /// A press that takes nothing says so on the screen — once, wherever it
    /// came from: the window does not say it a second time.
    #[test]
    fn a_step_not_taken_says_so_once() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (bus, channel, step) = step(&mut app);
        let solid = id(app.execute_command(EngineCommand::AddSolidColorDeck {
            channel_uuid: channel.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        }));
        let input = Some(BusInputRef::Deck(solid.clone()));
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownScreen {
                uuid: step.clone(),
                bus_uuid: bus.clone(),
                input,
            }),
            CommandResult::Ok
        ));
        // The bus mid-take elsewhere: the step cannot be faded to.
        for cmd in [
            EngineCommand::SetBusPreview {
                uuid: bus.clone(),
                input: BusInputRef::Black,
            },
            EngineCommand::BusAuto {
                uuid: bus.clone(),
                duration_secs: Some(60.0),
            },
        ] {
            assert!(matches!(app.execute_command(cmd), CommandResult::Ok));
        }
        let mut actions = crate::usecases::ui::UIActions::new();
        actions.commands.push(EngineCommand::TakeRundownStep {
            uuid: step.clone(),
            fade: true,
        });
        let _ = app.apply_engine_actions(&mut actions);
        let said: Vec<String> = notices(&app)
            .into_iter()
            .filter(|m| m.contains("not taken"))
            .collect();
        assert_eq!(said, ["Step 'B' not taken — still fading: PGM"]);
    }

    /// A change to a step that is refused says why in the window: the menu
    /// it was picked from has closed, and nothing else would show it.
    #[test]
    fn a_step_edit_that_is_refused_says_why() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, channel, step) = step(&mut app);
        let solid = id(app.execute_command(EngineCommand::AddSolidColorDeck {
            channel_uuid: channel,
            color: [1.0, 0.0, 0.0, 1.0],
        }));
        let mut actions = crate::usecases::ui::UIActions::new();
        actions.commands.push(EngineCommand::SetRundownPlay {
            uuid: step,
            deck_uuid: solid,
            action: Some(PlayAction::Start),
        });
        actions.commands.push(EngineCommand::AddRundownStep {
            name: "  ".into(),
            buses: None,
            main: false,
        });
        let _ = app.apply_engine_actions(&mut actions);
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m.starts_with("Step not changed: A step cannot start or stop that")),
            "{said:?}"
        );
        assert!(said.iter().any(|m| m == "Step not added: a step needs a name"), "{said:?}");
    }

    /// An input deleted from under the rundown: the steps that used it are
    /// named, rather than left to be found out on air.
    #[test]
    fn deleting_an_input_names_the_steps_that_used_it() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (bus, _, step) = step(&mut app);
        let extra = id(app.execute_command(EngineCommand::AddChannel));
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownScreen {
                uuid: step,
                bus_uuid: bus,
                input: Some(BusInputRef::Channel(extra.clone())),
            }),
            CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(EngineCommand::RemoveChannel {
                channel_uuid: extra
            }),
            CommandResult::Ok
        ));
        let said = notices(&app);
        assert!(
            said.iter().any(|m| m == "Rundown: B no longer uses what was deleted"),
            "{said:?}"
        );
    }

    /// A bus ticked for a new step that shows something not in the show is
    /// kept in the step as it is — to say so — not left out without a word.
    #[test]
    fn a_new_step_keeps_a_bus_whose_input_is_not_in_the_show() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let state = app.build_engine_state();
        let (bus, channel) = (
            state.buses[0].uuid.clone(),
            state.mixer.channels[0].uuid.clone(),
        );
        for cmd in [
            EngineCommand::SetBusPreview {
                uuid: bus.clone(),
                input: BusInputRef::Channel(channel.clone()),
            },
            EngineCommand::BusCut { uuid: bus.clone() },
        ] {
            assert!(matches!(app.execute_command(cmd), CommandResult::Ok));
        }
        // As after a show that opened in part: nothing the buses name can
        // be trusted.
        app.production_buses.quarantine_routing();
        app.reconcile_buses();
        let step = id(app.execute_command(EngineCommand::AddRundownStep {
            name: "A".into(),
            buses: Some(vec![bus.clone()]),
            main: false,
        }));
        let kept = app.rundown.step(&step).expect("the step").screen_sets();
        assert_eq!(kept, [(bus, BusInputRef::Channel(channel))]);
        let check = app.check_step(app.rundown.step(&step).expect("the step"));
        assert_eq!(check.missing.len(), 1, "{check:?}");
        let refused = app.execute_command(EngineCommand::TakeRundownStep {
            uuid: step,
            fade: false,
        });
        assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    }
    /// A step taken at its time even when not next: the clock comes to it
    /// with another step next, and the show goes there; GO goes on from it.
    #[test]
    fn a_step_at_its_time_even_when_not_next_is_taken_wherever_the_rundown_is() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, _, a) = step(&mut app);
        let mut add = |name: &str| {
            id(app.execute_command(EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            }))
        };
        let (b, c, d) = (add("B"), add("C"), add("D"));
        let quarter = 9 * 3600 + 900;
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownAt {
                uuid: d.clone(),
                at_secs: Some(quarter),
                always: Some(true),
            }),
            CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(EngineCommand::TakeRundownStep {
                uuid: a.clone(),
                fade: false,
            }),
            CommandResult::Ok
        ));
        assert_eq!(app.rundown.next().map(|s| s.uuid.clone()), Some(b.clone()));
        let now = app.frame_stats.now_secs();
        app.advance_rundown_by(now, f64::from(quarter) - 1.0);
        assert_eq!(app.rundown.last(), Some(a.as_str()), "not yet");
        app.advance_rundown_by(now + 0.5, f64::from(quarter) + 0.2);
        for i in 0..20 {
            app.advance_rundown_by(now + 1.0 + f64::from(i) * 0.1, f64::from(quarter) + 0.3);
        }
        assert_eq!(app.rundown.last(), Some(d.as_str()), "taken at 9:15, B and C left out");
        let _ = c;
    }

    /// What a wait for an end counts as playing: a clip started from its
    /// beginning only once that seek has happened (the run before, near its
    /// end, does not — however long the app was busy); one carried on at
    /// once — at its end already too; a playlist while it plays and is not
    /// at its end.
    #[test]
    fn a_wait_for_an_end_counts_a_clip_as_playing_once_it_has_been_sent_back() {
        let follow = |restarted: bool, seeks_before: Option<u64>| crate::rundown::Follow {
            until: Some("clip".into()),
            restarted,
            seeks_before,
            ..crate::rundown::Follow::at_for_tests()
        };
        let clip = |running: bool, ended: bool, seeks: u64| PlayState {
            running,
            ended,
            clip: Some(ClipAt { seeks }),
        };
        assert!(!seen_playing(&follow(true, Some(3)), &clip(true, false, 3)), "the run before");
        assert!(seen_playing(&follow(true, Some(3)), &clip(true, false, 4)), "sent back");
        // The app busy through the whole new run: played out already.
        assert!(seen_playing(&follow(true, Some(3)), &clip(false, true, 4)), "its own end");
        assert!(!seen_playing(&follow(true, None), &clip(false, true, 0)), "not seen at all");
        assert!(seen_playing(&follow(true, None), &clip(true, false, 0)), "not open before");
        assert!(seen_playing(&follow(false, Some(3)), &clip(false, true, 3)), "carried on");
        let list = |running: bool| PlayState { running, ended: !running, clip: None };
        assert!(seen_playing(&follow(false, None), &list(true)));
        assert!(!seen_playing(&follow(false, None), &list(false)));
    }

    /// A clip is at its end where its playhead stands — at its out-point —
    /// not by the end flag: left over from before a seek, it says nothing.
    #[test]
    fn a_clip_is_at_its_end_where_its_playhead_stands() {
        use crate::video::LoopMode;
        let mut snap = crate::video::PlaybackSnapshot::stopped();
        snap.duration = 10.0;
        snap.frame_rate = 25.0;
        snap.loop_mode = LoopMode::OneShot;
        snap.position = 10.0;
        snap.playing = false;
        assert!(super::clip_state(&snap).ended, "at its out-point");
        snap.position = 0.0;
        snap.playing = true;
        snap.reached_end = true;
        let state = super::clip_state(&snap);
        assert!(!state.ended && state.running, "back at its start: the old flag says nothing");
        snap.loop_mode = LoopMode::Loop;
        snap.position = 10.0;
        assert!(!super::clip_state(&snap).ended, "a loop never ends");
        // Its length not known yet: at no end.
        snap.loop_mode = LoopMode::OneShot;
        snap.duration = 0.0;
        snap.position = 0.0;
        assert!(!super::clip_state(&snap).ended, "no length yet");
    }

    /// Only a clip or a playlist ends: a timer is refused as what a step
    /// goes on at the end of.
    #[test]
    fn a_timer_is_not_what_a_step_goes_on_at_the_end_of() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, channel, step) = step(&mut app);
        let timer = id(app.execute_command(EngineCommand::AddTimerDeck {
            channel_uuid: channel,
            settings: None,
        }));
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownPlay {
                uuid: step.clone(),
                deck_uuid: timer.clone(),
                action: Some(PlayAction::Start),
            }),
            CommandResult::Ok
        ));
        let refused = app.execute_command(EngineCommand::SetRundownThen {
            uuid: step.clone(),
            secs: Some(5.0),
            when_ends: Some(timer),
        });
        assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
        assert_eq!(app.rundown.step(&step).unwrap().then_ends, None);
    }

    /// A step at a time of day is taken as the clock comes to it while it is
    /// next — faded, as GO takes it — and not while it is held.
    #[test]
    fn a_step_is_taken_at_its_time_of_day_and_not_while_held() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, _, a) = step(&mut app);
        let mut add = |name: &str| {
            id(app.execute_command(EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            }))
        };
        let (b, c, e) = (add("C"), add("D"), add("E"));
        let nine = 9 * 3600;
        for (step, at) in [(&b, nine), (&c, nine + 120)] {
            assert!(matches!(
                app.execute_command(EngineCommand::SetRundownAt {
                    uuid: step.clone(),
                    at_secs: Some(at),
                    always: None,
                }),
                CommandResult::Ok
            ));
        }
        assert!(matches!(
            app.execute_command(EngineCommand::TakeRundownStep {
                uuid: a.clone(),
                fade: false,
            }),
            CommandResult::Ok
        ));
        let now = app.frame_stats.now_secs();
        app.advance_rundown_by(now, f64::from(nine) - 1.0);
        assert_eq!(app.rundown.last(), Some(a.as_str()), "not yet");
        app.advance_rundown_by(now + 0.5, f64::from(nine) + 0.2);
        assert_eq!(app.rundown.last(), Some(b.as_str()), "taken at 9:00");

        // D is next, at 9:02. A step taken by hand and refused — it starts a
        // timer that shows the time of day — holds nothing: D goes at 9:02.
        let (_, channel, refused) = step(&mut app);
        let clock = id(app.execute_command(EngineCommand::AddTimerDeck {
            channel_uuid: channel,
            settings: None,
        }));
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownPlay {
                uuid: refused.clone(),
                deck_uuid: clock.clone(),
                action: Some(PlayAction::Start),
            }),
            CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(EngineCommand::SetTimerSettings {
                deck_uuid: clock,
                settings: crate::engine::value::timer::TimerSettings {
                    mode: crate::engine::value::timer::TimerMode::Clock,
                    ..Default::default()
                },
            }),
            CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(EngineCommand::TakeRundownStep {
                uuid: refused,
                fade: false,
            }),
            CommandResult::Err { .. }
        ));
        app.advance_rundown_by(now + 1.0, f64::from(nine + 119));
        app.advance_rundown_by(now + 1.5, f64::from(nine + 121));
        assert_eq!(app.rundown.last(), Some(c.as_str()), "taken at 9:02");

        // E is next, at 9:04: held, its time comes and goes without it.
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownAt {
                uuid: e.clone(),
                at_secs: Some(nine + 240),
                always: None,
            }),
            CommandResult::Ok
        ));
        assert!(matches!(
            app.execute_command(EngineCommand::RundownHold),
            CommandResult::Ok
        ));
        app.advance_rundown_by(now + 2.0, f64::from(nine + 239));
        app.advance_rundown_by(now + 2.5, f64::from(nine + 241));
        assert_eq!(app.rundown.last(), Some(c.as_str()), "held");
        assert_eq!(app.rundown.next().map(|s| s.uuid.as_str()), Some(e.as_str()));
    }
    /// A playlist in the show, for the fades to fade.
    fn playlist(app: &mut App) -> String {
        let (_, channel, _) = step(app);
        id(app.execute_command(EngineCommand::AddPlaylistDeck {
            channel_uuid: channel,
            items: vec![],
            settings: None,
        }))
    }

    fn fade(deck: &str, from: f64, secs: f32) -> super::SoundFade {
        super::SoundFade {
            deck: deck.into(),
            from,
            secs,
            then: PlayAction::Stop,
            start: 1.0,
            done_at: None,
            by_hand: false,
        }
    }

    fn list_playing(app: &App, deck: &str) -> bool {
        let (ch, dk) = app.resolve_deck(deck).unwrap();
        app.mixer.channels()[ch].decks[dk]
            .deck
            .playlist()
            .unwrap()
            .is_playing()
    }

    /// Stop pressed on a playing playlist fades its sound out over the
    /// Stop fade set — 2 s unless chosen — then pauses it where it is, and
    /// says it is stopping meanwhile.
    #[test]
    fn a_stop_fades_the_sound_out_then_pauses() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let music = playlist(&mut app);
        assert!(matches!(
            app.execute_command(EngineCommand::SetPlaylistPlaying {
                deck_uuid: music.clone(),
                playing: true,
            }),
            CommandResult::Ok
        ));
        assert!(list_playing(&app, &music));
        assert!(matches!(
            app.execute_command(EngineCommand::StopWithFade {
                deck_uuid: music.clone(),
                secs: None,
            }),
            CommandResult::Ok
        ));
        let fade = app.rundown_fades.iter().find(|f| f.deck == music).unwrap();
        assert!((fade.secs - crate::audio_mix::DEFAULT_STOP_FADE_SECS).abs() < 1e-6);
        assert_eq!(fade.then, PlayAction::Pause, "paused where it is, not rewound");
        let from = fade.from;
        assert_eq!(app.sounds_stopping(), vec![music.clone()]);
        assert!(list_playing(&app, &music), "still playing while it fades");
        app.run_sound_fades(from + 1.0);
        let half = app.audio_mix.step_fade(&music).unwrap();
        assert!((half - 0.5).abs() < 1e-3, "half way: {half}");
        // Just past its end: from + 2.0 - from can come out a hair short of 2.
        app.run_sound_fades(from + 2.01);
        assert!(!list_playing(&app, &music), "paused at the fade's end");
        assert!(app.sounds_stopping().is_empty());
    }

    /// A second Stop while it fades stops it at once; a Stop set to cut, or
    /// given 0, stops it at once; one not playing changes nothing.
    #[test]
    fn a_second_stop_or_a_cut_stops_at_once() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let music = playlist(&mut app);
        let play = |app: &mut App| {
            app.execute_command(EngineCommand::SetPlaylistPlaying {
                deck_uuid: music.clone(),
                playing: true,
            });
        };
        let stop = |app: &mut App, secs: Option<f32>| {
            assert!(matches!(
                app.execute_command(EngineCommand::StopWithFade {
                    deck_uuid: music.clone(),
                    secs,
                }),
                CommandResult::Ok
            ));
        };
        play(&mut app);
        stop(&mut app, None);
        assert!(list_playing(&app, &music), "fading");
        stop(&mut app, None);
        assert!(!list_playing(&app, &music), "a second Stop: at once");
        assert!(app.sounds_stopping().is_empty());

        app.rundown_fades.clear();
        play(&mut app);
        stop(&mut app, Some(0.0));
        assert!(!list_playing(&app, &music), "0 cuts");

        app.rundown_fades.clear();
        assert!(matches!(
            app.execute_command(EngineCommand::SetStopFade { secs: 0.0 }),
            CommandResult::Ok
        ));
        play(&mut app);
        stop(&mut app, None);
        assert!(!list_playing(&app, &music), "set to cut");
        assert!(app.rundown_fades.iter().all(|f| f.done_at.is_some()));

        app.rundown_fades.clear();
        stop(&mut app, None);
        assert!(app.rundown_fades.is_empty(), "not playing: nothing to fade");
    }

    /// The Stop fade is 0 to 10 seconds, kept with the show's sound
    /// settings; a Stop is refused on what has no sound to stop.
    #[test]
    fn the_stop_fade_is_kept_and_checked() {
        let Some(mut app) = headless_app() else {
            return;
        };
        for secs in [-1.0, 10.5, f32::NAN] {
            assert!(matches!(
                app.execute_command(EngineCommand::SetStopFade { secs }),
                CommandResult::Err { .. }
            ));
        }
        assert!((app.audio_mix.stop_fade_secs() - 2.0).abs() < 1e-6, "unchanged");
        assert!(matches!(
            app.execute_command(EngineCommand::SetStopFade { secs: 3.0 }),
            CommandResult::Ok
        ));
        let saved = serde_json::to_string(app.audio_mix.config()).unwrap();
        let loaded: crate::audio_mix::AudioMixConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded.stop_fade_secs, Some(3.0), "{saved}");
        let older: crate::audio_mix::AudioMixConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(older.stop_fade_secs, None, "a show from before: the default");
        assert!((build_engine_state_stop(&app) - 3.0).abs() < 1e-6);

        let (_, channel, _) = step(&mut app);
        let deck = id(app.execute_command(EngineCommand::AddSolidColorDeck {
            channel_uuid: channel,
            color: [1.0, 0.0, 0.0, 1.0],
        }));
        for (deck, why) in [(deck.as_str(), "a picture"), ("nosuchdk", "none")] {
            assert!(
                matches!(
                    app.execute_command(EngineCommand::StopWithFade {
                        deck_uuid: deck.to_string(),
                        secs: None,
                    }),
                    CommandResult::Err { .. }
                ),
                "{why}"
            );
        }
    }

    /// A 6-second 64x36 clip with a tone, or `None` without the ffmpeg tool.
    fn tone_clip(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let path = dir.join("tone.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=64x36:r=25:d=6")
            .args(["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=6"])
            .args(["-c:v", "mpeg4", "-c:a", "aac", "-shortest"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        made.then_some(path)
    }

    /// A clip deck on the show's first channel, playing.
    fn playing_clip(app: &mut App, path: std::path::PathBuf) -> Option<String> {
        let (_, channel, _) = step(app);
        let deck = id(app.execute_command(EngineCommand::AddVideoDeck {
            channel_uuid: channel,
            path,
        }));
        app.execute_command(EngineCommand::VideoTogglePlay {
            deck_uuid: deck.clone(),
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            app.render_mixer_frame();
            if app.video_playback_snapshot(&deck).is_some_and(|s| s.playing) {
                return Some(deck);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    /// Whether the clip still plays once the decoder has had a moment.
    fn still_playing(app: &mut App, deck: &str) -> bool {
        for _ in 0..30 {
            app.render_mixer_frame();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.video_playback_snapshot(deck).is_some_and(|s| s.playing)
    }

    /// A clip the timeline has a lane for is not the Stop's to fade: it is
    /// paused at once, as its pause always did — not refused.
    #[test]
    fn a_clip_on_the_timeline_is_paused_at_once() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = tone_clip(dir.path()) else {
            return;
        };
        let Some(deck) = playing_clip(&mut app, path) else {
            panic!("the clip did not start");
        };
        assert!(matches!(
            app.execute_command(EngineCommand::AddLane {
                deck_uuid: deck.clone(),
            }),
            CommandResult::Ok | CommandResult::OkWithId { .. }
        ));
        assert!(app.runs(&deck).is_err(), "precondition: the timeline's");
        let result = app.execute_command(EngineCommand::StopWithFade {
            deck_uuid: deck.clone(),
            secs: None,
        });
        assert!(matches!(result, CommandResult::Ok), "{result:?}");
        assert!(app.rundown_fades.is_empty(), "no fade of its own");
        assert!(!still_playing(&mut app, &deck), "paused at once");
    }

    /// A clip stopped by hand during an armed pass leaves its stop on the
    /// curve, as its pause did.
    #[test]
    fn a_stop_by_hand_is_taken_by_an_armed_pass() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = tone_clip(dir.path()) else {
            return;
        };
        let Some(deck) = playing_clip(&mut app, path) else {
            panic!("the clip did not start");
        };
        // Its own clock, not the transport's: with the transport rolling a
        // clip on Auto follows it, and its Stop is the timeline's pause.
        assert!(matches!(
            app.execute_command(EngineCommand::VideoSetTransportSync {
                deck_uuid: deck.clone(),
                sync: crate::engine::value::video::DeckTransportSync {
                    mode: crate::engine::value::video::TransportSyncMode::Never,
                    ..Default::default()
                },
            }),
            CommandResult::Ok
        ));
        app.set_record_armed(true);
        app.transport.tick(1.0);
        app.tick_recorder();
        let key = format!("deck_{deck}:{}", crate::video::modulation::PLAY);
        assert!(app.mixer_ref().modulation().assignments_for(&key).is_empty());
        assert!(matches!(
            app.execute_command(EngineCommand::StopWithFade {
                deck_uuid: deck.clone(),
                secs: None,
            }),
            CommandResult::Ok
        ));
        assert_eq!(app.sounds_stopping(), vec![deck.clone()], "the fade's way");
        assert!(
            still_playing(&mut app, &deck),
            "an armed pass does not cut the fade short"
        );
        let from = app.rundown_fades[0].from;
        app.transport.tick(1.0);
        app.run_sound_fades(from + f64::from(app.rundown_fades[0].secs) + 0.01);
        assert!(!still_playing(&mut app, &deck), "paused at the fade's end");
        app.tick_recorder();
        app.set_record_armed(false);
        assert!(
            !app.mixer_ref().modulation().assignments_for(&key).is_empty(),
            "the stop is on a curve for {key}"
        );
    }

    /// A clip of its own clock (not the transport's) during an armed pass.
    fn armed_free_clip(app: &mut App, deck: &str) {
        app.execute_command(EngineCommand::VideoSetTransportSync {
            deck_uuid: deck.to_string(),
            sync: crate::engine::value::video::DeckTransportSync {
                mode: crate::engine::value::video::TransportSyncMode::Never,
                ..Default::default()
            },
        });
        app.set_record_armed(true);
        app.transport.tick(1.0);
        app.tick_recorder();
    }

    fn play_curve(app: &App, deck: &str) -> bool {
        let key = format!("deck_{deck}:{}", crate::video::modulation::PLAY);
        !app.mixer_ref().modulation().assignments_for(&key).is_empty()
    }

    /// The PLAY curve recorded for a clip, as (position, value) points.
    fn play_points(app: &App, deck: &str) -> Vec<f32> {
        let key = format!("deck_{deck}:{}", crate::video::modulation::PLAY);
        let modulation = app.mixer_ref().modulation();
        let Some(source) = modulation.assignments_for(&key).first().map(|m| m.source_id.clone())
        else {
            return Vec::new();
        };
        match modulation.find_source_by_uuid(&source).map(|e| &e.source) {
            Some(crate::modulation::ModulationSource::Envelope { breakpoints, .. }) => {
                breakpoints.iter().map(|p| p.value).collect()
            }
            _ => Vec::new(),
        }
    }

    /// A Stop on a paused clip in an armed pass: noted only when there is a
    /// Play to undo — one noted this pass — never as a curve of its own,
    /// which would hold the clip paused when something later starts it.
    #[test]
    fn a_stop_on_a_paused_clip_is_noted_only_over_a_play() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = tone_clip(dir.path()) else {
            return;
        };
        let Some(deck) = playing_clip(&mut app, path) else {
            panic!("the clip did not start");
        };
        // Paused before the pass, by its own button.
        app.execute_command(EngineCommand::VideoTogglePlay {
            deck_uuid: deck.clone(),
        });
        assert!(!still_playing(&mut app, &deck));
        armed_free_clip(&mut app, &deck);
        let stop = |app: &mut App| {
            app.execute_command(EngineCommand::StopWithFade {
                deck_uuid: deck.clone(),
                secs: None,
            });
        };
        stop(&mut app);
        assert!(!play_curve(&app, &deck), "an idle Stop leaves no curve");

        // A Play noted this pass, its decoder not caught up: the Stop
        // undoes it on the curve too.
        app.note_live_video_write(&deck, crate::video::modulation::PLAY, 1.0);
        app.transport.tick(1.0);
        stop(&mut app);
        app.transport.tick(1.0);
        app.tick_recorder();
        app.set_record_armed(false);
        let points = play_points(&app, &deck);
        assert_eq!(points.last().copied(), Some(0.0), "ends paused: {points:?}");
    }

    /// A rundown fade that takes over from a Stop by hand before it lands
    /// keeps its gesture: the pause is noted when it lands.
    #[test]
    fn a_rundown_fade_over_a_hand_stop_keeps_its_gesture() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let Some(path) = tone_clip(dir.path()) else {
            return;
        };
        let Some(deck) = playing_clip(&mut app, path) else {
            panic!("the clip did not start");
        };
        armed_free_clip(&mut app, &deck);
        app.execute_command(EngineCommand::StopWithFade {
            deck_uuid: deck.clone(),
            secs: None,
        });
        let now = app.rundown_fades[0].from + 0.5;
        app.fade_out(&deck, now, Some(1.0), PlayAction::Stop);
        assert!(app.rundown_fades[0].by_hand, "kept through the takeover");
        app.run_sound_fades(now + 1.01);
        app.transport.tick(1.0);
        app.tick_recorder();
        app.set_record_armed(false);
        assert!(play_curve(&app, &deck), "noted when it landed");
    }

    fn build_engine_state_stop(app: &App) -> f32 {
        crate::app::snapshot::build_engine_state(app).audio_mix.stop_fade_secs
    }

    /// A sound faded out goes down with its fade, is stopped at its end and
    /// kept down a moment after — what its decoder had ready not heard at
    /// full level — then let go.
    #[test]
    fn a_sound_faded_out_is_kept_down_a_moment_after_it_stops() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let music = playlist(&mut app);
        app.rundown_fades.push(fade(&music, 10.0, 2.0));
        app.run_sound_fades(11.0);
        let half = app.audio_mix.step_fade(&music).unwrap();
        assert!((half - 0.5).abs() < 1e-6, "half way: {half}");
        app.run_sound_fades(12.0);
        assert_eq!(app.audio_mix.step_fade(&music), Some(0.0), "stopped, down");
        assert!(app.rundown_fades[0].done_at.is_some());
        app.run_sound_fades(12.0 + super::QUIET_AFTER_SECS * 0.5);
        assert_eq!(app.audio_mix.step_fade(&music), Some(0.0), "still down");
        app.run_sound_fades(12.0 + super::QUIET_AFTER_SECS + 0.01);
        assert_eq!(app.audio_mix.step_fade(&music), None, "let go");
        assert!(app.rundown_fades.is_empty());
    }

    /// A second fade of a sound already fading goes on from where the first
    /// had got to — never back up to full.
    #[test]
    fn a_fade_over_a_fade_goes_on_from_where_it_was() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let music = playlist(&mut app);
        app.rundown_fades.push(fade(&music, 10.0, 5.0));
        app.fade_out(&music, 14.0, Some(1.0), PlayAction::Pause);
        app.run_sound_fades(14.0);
        let now = app.audio_mix.step_fade(&music).unwrap();
        assert!((now - 0.2).abs() < 1e-5, "on from 0.2: {now}");
        app.run_sound_fades(14.5);
        let later = app.audio_mix.step_fade(&music).unwrap();
        assert!((later - 0.1).abs() < 1e-5, "half of 0.2 later: {later}");
    }

    /// A deck deleted while it fades is let go — its fade does not turn down
    /// or stop what comes back under its UUID — and a show opened lets go
    /// of the fades of the one before.
    #[test]
    fn a_fade_is_let_go_with_its_deck_and_with_its_show() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let music = playlist(&mut app);
        app.rundown_fades.push(fade(&music, 10.0, 5.0));
        app.run_sound_fades(11.0);
        assert!(app.audio_mix.step_fade(&music).is_some());
        assert!(matches!(
            app.execute_command(EngineCommand::RemoveDeck {
                deck_uuid: music.clone(),
            }),
            CommandResult::Ok
        ));
        assert!(app.rundown_fades.is_empty(), "let go with its deck, at once");
        app.run_sound_fades(11.5);
        assert_eq!(app.audio_mix.step_fade(&music), None);

        // A show opening stops what was fading out — as its fade would have
        // — before letting go: here a timer counting, reset.
        let (_, channel, _) = step(&mut app);
        let timer = id(app.execute_command(EngineCommand::AddTimerDeck {
            channel_uuid: channel,
            settings: None,
        }));
        assert!(matches!(
            app.execute_command(EngineCommand::ControlTimer {
                deck_uuid: timer.clone(),
                action: crate::engine::value::timer::TimerAction::Start,
            }),
            CommandResult::Ok
        ));
        let running = |app: &App| {
            let (ch, dk) = app.resolve_deck(&timer).unwrap();
            app.mixer.channels()[ch].decks[dk].deck.timer().unwrap().running()
        };
        assert!(running(&app));
        app.rundown_fades.push(fade(&timer, 10.0, 5.0));
        app.run_sound_fades(11.0);
        app.forget_rundown_live();
        assert!(!running(&app), "stopped as its fade would have");
        assert_eq!(app.audio_mix.step_fade(&timer), Some(0.0), "kept down a moment");
        assert!(app.rundown_fades.iter().all(|f| f.done_at.is_some()));
        let later = app.frame_stats.now_secs() + super::QUIET_AFTER_SECS + 0.01;
        app.run_sound_fades(later);
        assert!(app.rundown_fades.is_empty(), "then let go");
        assert_eq!(app.audio_mix.step_fade(&timer), None);
        // Which a show opening does; once its scene is in place, nothing is
        // kept down — its decks are new, under the same UUIDs here.
        let layout = crate::usecases::ui::UILayoutState::default();
        app.save_workspace(&layout).unwrap();
        app.flush_saves();
        app.rundown_fades.push(fade(&timer, 10.0, 5.0));
        app.run_sound_fades(11.0);
        let _ = app.load_workspace();
        assert!(app.rundown_fades.is_empty(), "let go with its show");
        assert_eq!(app.audio_mix.step_fade(&timer), None, "the new decks not kept down");
    }

    /// A step made next after the clock read its time — by GO takes this
    /// next — is not taken by that time: its time is counted from when it
    /// is next.
    #[test]
    fn a_step_made_next_after_its_time_is_not_taken_by_it() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, _, a) = step(&mut app);
        let b = id(app.execute_command(EngineCommand::AddRundownStep {
            name: "C".into(),
            buses: Some(vec![]),
            main: false,
        }));
        let nine = 9 * 3600;
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownAt {
                uuid: b.clone(),
                at_secs: Some(nine),
                always: None,
            }),
            CommandResult::Ok
        ));
        // A next, read just before 9:00; B made next just after.
        let now = app.frame_stats.now_secs();
        app.advance_rundown_by(now, f64::from(nine) - 0.01);
        assert_eq!(app.rundown.next().map(|s| s.uuid.clone()), Some(a));
        assert!(matches!(
            app.execute_command(EngineCommand::SetRundownNext { uuid: b.clone() }),
            CommandResult::Ok
        ));
        app.advance_rundown_by(now + 0.1, f64::from(nine) + 0.02);
        assert_eq!(app.rundown.last(), None, "not taken");
    }
    /// A step made next by a wait taken just before its time goes by its time
    /// on the next frame: the time read is kept with the step next once the
    /// frame is done.
    #[test]
    fn a_step_made_next_by_a_wait_just_before_its_time_is_taken_at_it() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let (_, _, first) = step(&mut app);
        let mut add = |name: &str| {
            id(app.execute_command(EngineCommand::AddRundownStep {
                name: name.into(),
                buses: Some(vec![]),
                main: false,
            }))
        };
        let (a, b) = (add("A"), add("B"));
        let nine = 9 * 3600;
        for cmd in [
            EngineCommand::SetRundownThen {
                uuid: first.clone(),
                secs: Some(0.1),
                when_ends: None,
            },
            EngineCommand::SetRundownAt {
                uuid: b.clone(),
                at_secs: Some(nine),
                always: None,
            },
            EngineCommand::TakeRundownStep {
                uuid: first,
                fade: false,
            },
        ] {
            assert!(matches!(app.execute_command(cmd), CommandResult::Ok));
        }
        let now = app.frame_stats.now_secs();
        // Its wait runs out at 08:59:59.99: A is taken, B next.
        app.advance_rundown_by(now + 1.0, f64::from(nine) - 0.01);
        assert_eq!(app.rundown.last(), Some(a.as_str()));
        // 09:00:00.01: B, at its time.
        app.advance_rundown_by(now + 1.02, f64::from(nine) + 0.01);
        assert_eq!(app.rundown.last(), Some(b.as_str()), "taken at 9:00");
    }
}

