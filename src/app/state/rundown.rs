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
use crate::engine::value::timer::TimerAction;
use crate::engine::{CommandResult, ErrorCode};
use crate::mixer::{MainSource, MixMode};
use crate::rundown::{OverlaySet, PlayAction, ScreenSet, Step};

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
            return if timer.settings().mode.has_transport() {
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
            if let Err(why) = self.runs(&play.deck) {
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
            wait: rundown.follow().map(|follow| RundownWaitSnapshot {
                from: follow.from.clone(),
                to: follow.to.clone(),
                secs_left: (follow.due - now).max(0.0) as f32,
                fade: follow.fade,
            }),
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
        let held = self.clips_held_by(&step);
        self.clips_held
            .retain(|(uuid, _, _)| !held.iter().any(|(again, _, _)| again == uuid));
        self.clips_held.extend(held);
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
        for play in &step.plays {
            self.run_play(&play.deck, play.action);
        }
        self.rundown.taken(uuid, fade, now, settle);
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

    /// Start, carry on, pause or stop a clip, a playlist or a timer. One
    /// that cannot be — [`Self::check_step`] has said so — is passed over.
    fn run_play(&mut self, deck_uuid: &str, action: PlayAction) {
        let Ok(runs) = self.runs(deck_uuid) else {
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
                let _ = self.edit_timer(deck_uuid, |timer| match action {
                    PlayAction::Start => {
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
        self.rundown.hold();
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
        self.rundown.hold();
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

    /// How long after a step the next is taken by itself, or with `None`
    /// not until the operator says.
    pub fn cmd_set_rundown_then(&mut self, uuid: &str, secs: Option<f32>) -> CommandResult {
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

    /// Drop the wait of a step that goes on by itself.
    pub fn cmd_rundown_hold(&mut self) -> CommandResult {
        if self.rundown.hold() {
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
}
