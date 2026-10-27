//! The rundown: the show in order, as steps prepared ahead and taken with
//! one press.
//!
//! A conference follows a running order — the opening, a walk-on, a talk, a
//! panel — and each moment wants several things at once: every part of the
//! LED wall changed together, a camera taken to Main, a sting started (an
//! operator, 2026-10-13: "so i could set up all and then q just start a (play
//! audio, show on screen then transition camera to program etc)"). A
//! [`Step`] is one such moment; the [`Rundown`] keeps them in order and
//! knows which comes next.
//!
//! This module is the list and its bookkeeping only. Taking a step — the
//! buses, Main, the clips — is the app's (`app/state/rundown.rs`), which
//! reads a step and acts on the mixer.
//!
//! Not the arrangement's cue points (`crate::arrangement::Cue`): those mark
//! places on the timeline.

use crate::bus::BusInputRef;
use crate::mixer::MainSource;

/// Seconds the screens take to fade to a step, until set.
pub const DEFAULT_FADE_SECS: f32 = 1.0;
/// The longest a step waits before going on to the next by itself: an hour.
pub const MAX_THEN_SECS: f32 = 3600.0;

/// What one bus shows in a step.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ScreenSet {
    /// The bus, by UUID.
    pub bus: String,
    /// What it shows.
    pub input: BusInputRef,
}

/// An overlay channel a step switches on or off over Main.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct OverlaySet {
    /// The overlay channel, by UUID.
    pub channel: String,
    /// On, or off.
    pub on: bool,
}

/// What a step does to something that runs: a clip, a playlist or a timer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlayAction {
    /// Start it from its beginning: a clip from its in-point, a playlist
    /// from its first item, a timer from its top.
    Start,
    /// Carry on from where it is.
    Resume,
    /// Stop it where it is.
    Pause,
    /// Stop it, back at its beginning.
    Stop,
}

/// How a step sets a timer it starts: counting down from a length of its
/// own, or up — a talk of 15 minutes, the next of 20, a panel counted up.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TimerSet {
    /// Down from this many seconds.
    Down { secs: u32 },
    /// Up from zero.
    Up,
}

/// A clip, playlist or timer a step starts or stops.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PlaySet {
    /// Its deck, by UUID.
    pub deck: String,
    /// What is done to it.
    pub action: PlayAction,
    /// A clip or playlist it starts or carries on: faded out — its sound
    /// over the fade of the step taken next — and stopped, back at its
    /// beginning, when another step is taken that does not start or stop it
    /// itself. Music for one moment of the show.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub until_next: bool,
    /// A timer it starts: set so as it starts — counting down from its own
    /// length, or up; as the timer is set when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer: Option<TimerSet>,
}

impl PlaySet {
    /// `action` done to `deck`, nothing more.
    pub fn new(deck: impl Into<String>, action: PlayAction) -> Self {
        Self {
            deck: deck.into(),
            action,
            until_next: false,
            timer: None,
        }
    }
}

/// One moment of the show: what every screen shows, what is on Main, which
/// clips start. Whatever a step does not set is left as it is.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Step {
    /// Stable UUID.
    pub uuid: String,
    /// What the operator calls it: "A", "B Walk-on".
    pub name: String,
    /// What each bus shows. A bus not listed is left as it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<ScreenSet>,
    /// What goes to Main; `None` leaves Main as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<MainSource>,
    /// Overlay channels switched on or off. One not listed is left as it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<OverlaySet>,
    /// Clips, playlists and timers started or stopped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plays: Vec<PlaySet>,
    /// Go on to the next step by itself this many seconds after this one is
    /// taken — more than none, an hour at most; `None` waits for the
    /// operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then_secs: Option<f32>,
    /// The time of day it is taken by itself at — faded to, as GO takes it
    /// — while it is the step GO takes next: seconds after midnight. `None`
    /// waits for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_secs: Option<u32>,
}

/// Seconds in a day: a time of day is fewer.
pub const DAY_SECS: u32 = 24 * 3600;

impl Step {
    /// A step called `name` that sets nothing yet.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            uuid: crate::deck::generate_short_uuid(),
            name: name.into(),
            screens: Vec::new(),
            main: None,
            overlays: Vec::new(),
            plays: Vec::new(),
            then_secs: None,
            at_secs: None,
        }
    }

    /// What its screens are set to, as [`crate::bus::BusBank::take_together`]
    /// takes them.
    pub fn screen_sets(&self) -> Vec<(String, BusInputRef)> {
        self.screens
            .iter()
            .map(|s| (s.bus.clone(), s.input.clone()))
            .collect()
    }

    /// Whether it sets nothing at all.
    pub fn is_empty(&self) -> bool {
        self.screens.is_empty()
            && self.main.is_none()
            && self.overlays.is_empty()
            && self.plays.is_empty()
    }

    /// What it sets on the buses, Main and the overlays: its picture.
    pub fn sets_picture(&self) -> bool {
        !self.screens.is_empty() || self.main.is_some() || !self.overlays.is_empty()
    }

    /// Let go of a channel or deck that was deleted. Whether anything
    /// changed.
    fn forget_source(&mut self, uuid: &str) -> bool {
        let before = (
            self.screens.len(),
            self.main.is_some(),
            self.overlays.len(),
            self.plays.len(),
        );
        self.screens.retain(|s| !s.input.addresses(uuid));
        if self.main.as_ref().and_then(MainSource::uuid) == Some(uuid) {
            self.main = None;
        }
        self.overlays.retain(|o| o.channel != uuid);
        self.plays.retain(|p| p.deck != uuid);
        before
            != (
                self.screens.len(),
                self.main.is_some(),
                self.overlays.len(),
                self.plays.len(),
            )
    }

    /// One entry per bus, overlay and deck — the first kept — and a wait
    /// that is a time: what a file written by hand could get wrong.
    fn tidy(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.screens.retain(|s| seen.insert(s.bus.clone()));
        seen.clear();
        self.overlays.retain(|o| seen.insert(o.channel.clone()));
        seen.clear();
        self.plays.retain(|p| seen.insert(p.deck.clone()));
        for play in &mut self.plays {
            play.timer = play.timer.map(valid_timer);
        }
        self.then_secs = self.then_secs.and_then(valid_then);
        self.at_secs = self.at_secs.filter(|secs| *secs < DAY_SECS);
    }
}

/// A timer setting as kept: a count down of a second to a timer's longest.
pub fn valid_timer(set: TimerSet) -> TimerSet {
    match set {
        TimerSet::Down { secs } => TimerSet::Down {
            secs: secs.clamp(1, crate::engine::value::timer::MAX_DURATION_SECS),
        },
        TimerSet::Up => TimerSet::Up,
    }
}

/// How long after its time of day a step can still be taken by it: a Mac
/// waking, or Nida opened, after the time does not take a step long due.
pub const AT_GRACE_SECS: f64 = 60.0;

/// A `then` wait as kept: more than none and within an hour, or nothing for
/// one that is not a time.
pub fn valid_then(secs: f32) -> Option<f32> {
    (secs.is_finite() && secs > 0.0).then(|| secs.min(MAX_THEN_SECS))
}

/// A step's wait being counted out: when it is due, [`Follow::to`] is taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Follow {
    /// The step whose wait this is, by UUID.
    pub from: String,
    /// The step it leads to, by UUID: the one that followed `from` when
    /// `from` was taken. Fixed then, so the operator sees where the wait
    /// leads and nothing done to the list meanwhile sends it elsewhere.
    pub to: String,
    /// The frame-clock reading, in seconds, it is due at.
    pub due: f64,
    /// Whether `to` is faded to, as `from` was, or cut.
    pub fade: bool,
}

/// The rundown as saved in the stage file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RundownConfig {
    /// The steps, in show order.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Seconds the screens take to fade to a step.
    #[serde(default = "default_fade_secs")]
    pub fade_secs: f32,
    /// The step taken last, by UUID: where the show is. It may name a step
    /// since removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    /// The step the operator said comes next, by UUID, where that is not
    /// simply the one after `last`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// With `last` removed: the steps still to come, by UUID.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to_come: Vec<String>,
}

fn default_fade_secs() -> f32 {
    DEFAULT_FADE_SECS
}

/// The steps in show order, the fade time, and where the show is.
#[derive(Debug, Clone)]
pub struct Rundown {
    steps: Vec<Step>,
    fade_secs: f32,
    /// The step taken last: `GO` takes the one after it. Kept when that step
    /// is removed, so that undoing the removal puts the show back on it.
    last: Option<String>,
    /// While the step taken last is removed: the steps that stood after it
    /// then, and those made since — what is still to come. `GO` takes the
    /// first of them in the list as it is now, and nothing when none is
    /// left: not a step the show had passed, however steps are moved,
    /// removed and put back meanwhile. Once a step is taken, the show is on
    /// it, and `GO` goes by the list from there as ever.
    to_come: Vec<String>,
    /// The step the operator said `GO` takes instead, until a step is taken.
    next: Option<String>,
    follow: Option<Follow>,
    /// The step whose time of day the operator held: not taken by itself at
    /// it, till a step is taken, another is made next, or its time changed.
    at_held: Option<String>,
}

impl Default for Rundown {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            fade_secs: DEFAULT_FADE_SECS,
            last: None,
            to_come: Vec::new(),
            next: None,
            follow: None,
            at_held: None,
        }
    }
}

impl Rundown {
    /// The steps, in show order.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The step with this UUID.
    pub fn step(&self, uuid: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.uuid == uuid)
    }

    fn position(&self, uuid: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.uuid == uuid)
    }

    /// Seconds the screens take to fade to a step.
    pub fn fade_secs(&self) -> f32 {
        self.fade_secs
    }

    /// Set the fade time. Whether `secs` was a time: finite and positive.
    pub fn set_fade_secs(&mut self, secs: f32) -> bool {
        let valid = secs.is_finite() && secs > 0.0;
        if valid {
            self.fade_secs = secs;
        }
        valid
    }

    /// The step taken last, by UUID — none taken, or the one taken since
    /// removed.
    pub fn last(&self) -> Option<&str> {
        self.last.as_deref().filter(|uuid| self.position(uuid).is_some())
    }


    /// The step `GO` takes: the one the operator said comes next, else the
    /// one after the step taken last, the first when none was taken — and
    /// none at the end, so `GO` after the last step never starts the show
    /// again. With the step taken last removed, the first of those that
    /// were still to come.
    pub fn next(&self) -> Option<&Step> {
        if let Some(step) = self.next.as_deref().and_then(|uuid| self.step(uuid)) {
            return Some(step);
        }
        match (&self.last, self.last()) {
            (None, _) => self.steps.first(),
            (Some(_), Some(last)) => self.after(last),
            (Some(_), None) => self.steps.iter().find(|s| self.to_come.contains(&s.uuid)),
        }
    }

    /// The step after this one in the list.
    fn after(&self, uuid: &str) -> Option<&Step> {
        self.steps.get(self.position(uuid)? + 1)
    }

    /// The wait being counted out, if a step's `then` is running.
    pub fn follow(&self) -> Option<&Follow> {
        self.follow.as_ref()
    }

    /// Whether the step taken last has been removed since.
    fn last_is_removed(&self) -> bool {
        self.last.is_some() && self.last().is_none()
    }

    /// Add a step at the end, returning its UUID.
    pub fn add(&mut self, mut step: Step) -> String {
        step.tidy();
        let uuid = step.uuid.clone();
        self.steps.push(step);
        // At the end of the list, it is still to come.
        if self.last_is_removed() {
            self.to_come.push(uuid.clone());
        }
        self.follow = None;
        uuid
    }

    /// A copy of a step, put right after it, to change into the next
    /// moment: its UUID, or `None` when there is no such step.
    pub fn duplicate(&mut self, uuid: &str) -> Option<String> {
        let idx = self.position(uuid)?;
        let copy = Step {
            uuid: crate::deck::generate_short_uuid(),
            name: format!("{} copy", self.steps[idx].name),
            ..self.steps[idx].clone()
        };
        let new = copy.uuid.clone();
        self.steps.insert(idx + 1, copy);
        // Right after a step still to come, it is still to come.
        if self.last_is_removed() && self.to_come.iter().any(|u| u == uuid) {
            self.to_come.push(new.clone());
        }
        self.follow = None;
        Some(new)
    }

    /// Remove a step. `GO` goes on from where it was — taking what followed
    /// the step the show was on, if that is the one removed — and a wait is
    /// dropped.
    pub fn remove(&mut self, uuid: &str) -> Option<Step> {
        let idx = self.position(uuid)?;
        let order = self.order();
        let step = self.steps.remove(idx);
        self.settle(&order);
        Some(step)
    }

    /// Rename a step. Whether there was one.
    pub fn rename(&mut self, uuid: &str, name: &str) -> bool {
        self.edit(uuid, |step| step.name = name.to_string())
    }

    /// Move a step one place later or earlier. Whether it moved.
    pub fn move_step(&mut self, uuid: &str, later: bool) -> bool {
        let Some(idx) = self.position(uuid) else {
            return false;
        };
        let to = if later {
            idx + 1
        } else {
            idx.wrapping_sub(1)
        };
        if to >= self.steps.len() {
            return false;
        }
        self.steps.swap(idx, to);
        self.follow = None;
        true
    }

    /// Change a step. A wait being counted out is dropped: what it would
    /// lead to is no longer what the operator saw when it started.
    fn edit(&mut self, uuid: &str, change: impl FnOnce(&mut Step)) -> bool {
        let Some(step) = self.steps.iter_mut().find(|s| s.uuid == uuid) else {
            return false;
        };
        change(step);
        self.follow = None;
        true
    }

    /// What `bus` shows in a step, or with `None` leave that bus as it is.
    /// Whether there was such a step.
    pub fn set_screen(&mut self, uuid: &str, bus: &str, input: Option<BusInputRef>) -> bool {
        self.edit(uuid, |step| {
            let at = step.screens.iter().position(|s| s.bus == bus);
            match (at, input) {
                (Some(at), Some(input)) => step.screens[at].input = input,
                (Some(at), None) => {
                    step.screens.remove(at);
                }
                (None, Some(input)) => step.screens.push(ScreenSet {
                    bus: bus.to_string(),
                    input,
                }),
                (None, None) => {}
            }
        })
    }

    /// What a step takes to Main, or with `None` leave Main as it is.
    pub fn set_main(&mut self, uuid: &str, main: Option<MainSource>) -> bool {
        self.edit(uuid, |step| step.main = main)
    }

    /// Whether a step switches an overlay channel on or off, or with `None`
    /// leaves it as it is.
    pub fn set_overlay(&mut self, uuid: &str, channel: &str, on: Option<bool>) -> bool {
        self.edit(uuid, |step| {
            step.overlays.retain(|o| o.channel != channel);
            if let Some(on) = on {
                step.overlays.push(OverlaySet {
                    channel: channel.to_string(),
                    on,
                });
            }
        })
    }

    /// What a step does to a clip, a playlist or a timer, or with `None`
    /// nothing.
    pub fn set_play(&mut self, uuid: &str, deck: &str, action: Option<PlayAction>) -> bool {
        self.edit(uuid, |step| {
            let at = step.plays.iter().position(|p| p.deck == deck);
            match (at, action) {
                (Some(at), Some(action)) => step.plays[at].action = action,
                (Some(at), None) => {
                    step.plays.remove(at);
                }
                (None, Some(action)) => step.plays.push(PlaySet::new(deck, action)),
                (None, None) => {}
            }
        })
    }

    /// Whether a clip or playlist a step starts or carries on is faded out
    /// and stopped when another step is taken. Whether the step has that
    /// clip or playlist among what it starts and stops.
    pub fn set_until_next(&mut self, uuid: &str, deck: &str, on: bool) -> bool {
        let there = self
            .step(uuid)
            .is_some_and(|step| step.plays.iter().any(|p| p.deck == deck));
        there
            && self.edit(uuid, |step| {
                for play in step.plays.iter_mut().filter(|p| p.deck == deck) {
                    play.until_next = on;
                }
            })
    }

    /// How a step sets a timer it starts, or with `None` as the timer is
    /// set. Whether the step has that timer among what it starts and stops.
    pub fn set_timer(&mut self, uuid: &str, deck: &str, timer: Option<TimerSet>) -> bool {
        let there = self
            .step(uuid)
            .is_some_and(|step| step.plays.iter().any(|p| p.deck == deck));
        there
            && self.edit(uuid, |step| {
                for play in step.plays.iter_mut().filter(|p| p.deck == deck) {
                    play.timer = timer.map(valid_timer);
                }
            })
    }

    /// How long after a step the next is taken by itself, or with `None`
    /// not at all. A wait that is not a time (see [`valid_then`]) counts as
    /// none.
    pub fn set_then(&mut self, uuid: &str, secs: Option<f32>) -> bool {
        self.edit(uuid, |step| step.then_secs = secs.and_then(valid_then))
    }

    /// The time of day a step is taken by itself at, seconds after midnight,
    /// or with `None` not at all — a time past the day counting as none. A
    /// hold on its time is let go: the new one stands.
    pub fn set_at(&mut self, uuid: &str, at_secs: Option<u32>) -> bool {
        if self.at_held.as_deref() == Some(uuid) {
            self.at_held = None;
        }
        self.edit(uuid, |step| step.at_secs = at_secs.filter(|s| *s < DAY_SECS))
    }

    /// The step whose time of day the operator held, by UUID.
    pub fn at_held(&self) -> Option<&str> {
        self.at_held.as_deref()
    }

    /// The step due by its time of day: the step `GO` takes next, with a
    /// time the clock has just come to — from `was` to `now`, seconds after
    /// local midnight, read a frame apart — not held, and no more than
    /// [`AT_GRACE_SECS`] past it.
    ///
    /// A reading lower than the one before is midnight only when it is half
    /// a day lower: then `was` is of the day before, and so may be a time
    /// just before midnight. Any other step back — the clock set back,
    /// summer time ending — takes nothing.
    pub fn due_at(&self, was: f64, now: f64) -> Option<&Step> {
        let step = self.next()?;
        let at = f64::from(step.at_secs?);
        if self.at_held.as_deref() == Some(step.uuid.as_str()) {
            return None;
        }
        let day = f64::from(DAY_SECS);
        let was = if was <= now {
            was
        } else if was - now > day / 2.0 {
            was - day
        } else {
            return None;
        };
        let come_to = |at: f64| was < at && at <= now && now - at <= AT_GRACE_SECS;
        (come_to(at) || come_to(at - day)).then_some(step)
    }

    /// The step `uuid`, due by its time of day, to be taken now — at `now`
    /// on the frame clock — as a wait run out is: faded to, tried again
    /// while something still lands, and said when it cannot be.
    pub fn start_at(&mut self, uuid: &str, now: f64) -> bool {
        if self.position(uuid).is_none() {
            return false;
        }
        self.follow = Some(Follow {
            from: self.last.clone().unwrap_or_default(),
            to: uuid.to_string(),
            due: now,
            fade: true,
        });
        true
    }

    /// Replace a step's picture with `with`'s — its screens, Main and
    /// overlays; its name, what it starts and its wait stay.
    pub fn set_picture(&mut self, uuid: &str, with: Step) -> bool {
        self.edit(uuid, |step| {
            step.screens = with.screens;
            step.main = with.main;
            step.overlays = with.overlays;
            step.tidy();
        })
    }

    /// A step was taken at `now` (the frame clock, in seconds): the show is
    /// there, `GO` takes the one after it, and its wait, if it has one and a
    /// step follows it, starts — replacing any running. Whether there was
    /// such a step.
    ///
    /// `settle_secs` is how long the take's own fades run. The wait is never
    /// shorter: a step set to go on after two seconds, faded to over three,
    /// would ask for the next fade in the middle of its own, and be refused
    /// every time.
    pub fn taken(&mut self, uuid: &str, fade: bool, now: f64, settle_secs: f32) -> bool {
        let Some(step) = self.step(uuid) else {
            return false;
        };
        self.follow = step.then_secs.zip(self.after(uuid)).map(|(secs, to)| Follow {
            from: uuid.to_string(),
            to: to.uuid.clone(),
            due: now + f64::from(secs.max(settle_secs)),
            fade,
        });
        self.last = Some(uuid.to_string());
        self.to_come.clear();
        self.next = None;
        self.at_held = None;
        true
    }

    /// Make `GO` take this step next, without taking anything: the step
    /// taken last stays where the show is. The operator has taken the order
    /// into their own hands, so a wait being counted out is dropped — and a
    /// hold on a time of day let go, the step next now going by its own.
    /// Whether there was such a step.
    pub fn set_next(&mut self, uuid: &str) -> bool {
        if self.position(uuid).is_none() {
            return false;
        }
        self.next = Some(uuid.to_string());
        self.follow = None;
        self.at_held = None;
        true
    }

    /// The wait that has run out, if one has: the step it leads to and
    /// whether to fade to it are in it, and how long it is overdue is
    /// `now - due`. It stays until [`Self::taken`] or [`Self::hold`]: whoever
    /// takes the step says when to give up on one that cannot be taken yet.
    pub fn due(&self, now: f64) -> Option<&Follow> {
        self.follow.as_ref().filter(|follow| now >= follow.due)
    }

    /// Drop a wait being counted out: a step taken by hand, or the step it
    /// led to being taken. Whether there was one.
    pub fn drop_wait(&mut self) -> bool {
        self.follow.take().is_some()
    }

    /// The operator's Hold: drop a wait being counted out, and hold the time
    /// of day of the step `GO` takes next, if it has one — the show waits
    /// for the operator. Whether there was either to stop.
    pub fn hold(&mut self) -> bool {
        let timed = self
            .next()
            .filter(|step| step.at_secs.is_some())
            .map(|step| step.uuid.clone());
        let held = timed.is_some() && self.at_held != timed;
        if held {
            self.at_held = timed;
        }
        self.follow.take().is_some() || held
    }

    /// A bus was removed: no step sets it any more. Whether anything changed.
    pub fn forget_bus(&mut self, bus: &str) -> bool {
        let mut changed = false;
        for step in &mut self.steps {
            let before = step.screens.len();
            step.screens.retain(|s| s.bus != bus);
            changed |= step.screens.len() != before;
        }
        if changed {
            self.follow = None;
        }
        changed
    }

    /// A channel or deck was deleted: steps let go of it, as buses do (see
    /// [`crate::bus::BusBank::forget`]). The names of the steps that
    /// changed.
    pub fn forget_source(&mut self, uuid: &str) -> Vec<String> {
        let changed: Vec<String> = self
            .steps
            .iter_mut()
            .filter_map(|step| step.forget_source(uuid).then(|| step.name.clone()))
            .collect();
        if !changed.is_empty() {
            self.follow = None;
        }
        changed
    }

    /// The steps as undo or redo puts them back — `then` being the rundown
    /// as it was saved with them — leaving what is live alone: the fade
    /// time, and where the show is. The show is on the step taken last if
    /// that is there again. With it still removed, what is to come is as it
    /// was with these steps, if the show was on the same step then: a step
    /// made while it was removed, undone and redone, is to come again. A
    /// wait is dropped.
    pub fn restore_steps(&mut self, then: &RundownConfig) {
        let order = self.order();
        self.steps = unique(then.steps.clone());
        self.settle(&order);
        if self.last_is_removed() && then.last == self.last {
            self.to_come.clone_from(&then.to_come);
        }
    }

    fn order(&self) -> Vec<String> {
        self.steps.iter().map(|s| s.uuid.clone()).collect()
    }

    /// After the list changed under the show, `order` being the steps as
    /// they were. The step taken last is remembered even when it has gone —
    /// undo may put it back, and the show is on it again — along with the
    /// steps that stood after it then, which are what `GO` has yet to take
    /// (see [`Self::next`]). A next that is gone is forgotten; a wait is
    /// dropped.
    fn settle(&mut self, order: &[String]) {
        let last_there = self.last.as_ref().map(|last| self.position(last).is_some());
        match (&self.last, last_there) {
            // Gone: if it was there until now, what stood after it; gone
            // before this change, what was to come then still is.
            (Some(last), Some(false)) => {
                if let Some(was) = order.iter().position(|u| u == last) {
                    self.to_come = order[was + 1..].to_vec();
                }
            }
            // There, or there again — the show is on it — or none taken.
            _ => self.to_come.clear(),
        }
        if self.next.as_ref().is_some_and(|n| self.position(n).is_none()) {
            self.next = None;
        }
        if self.at_held.as_ref().is_some_and(|h| self.position(h).is_none()) {
            self.at_held = None;
        }
        self.follow = None;
    }

    /// The rundown as saved.
    pub fn to_config(&self) -> RundownConfig {
        // With the step taken last removed, the show is written as on that
        // step still — a step that is not there, so no step is marked — with
        // the steps still to come: a restart has no undo to put it back.
        let to_come = if self.last_is_removed() {
            let there = |uuid: &&String| self.position(uuid).is_some();
            self.to_come.iter().filter(there).cloned().collect()
        } else {
            Vec::new()
        };
        RundownConfig {
            steps: self.steps.clone(),
            fade_secs: self.fade_secs,
            last: self.last.clone(),
            next: self.next.clone(),
            to_come,
        }
    }

    /// A rundown from a saved one. What a file could get wrong is put right
    /// rather than refused, so one bad field cannot keep a show from
    /// opening: a step without a UUID or with another's is dropped, a fade
    /// time that is not a time is the default, a next that is no step is
    /// none. A step taken last that is not in the list is a step since
    /// removed: no step is marked, and `GO` takes the first of those written
    /// as still to come, or nothing — never the top of the rundown, which a
    /// show that had got further must not start again. A bus or an input a step names that is
    /// not there is kept, so the show as meant is not lost: the step says so
    /// and is not taken until put right. No wait is running.
    pub fn from_config(config: RundownConfig) -> Self {
        let steps = unique(config.steps);
        let next = config.next.filter(|u| steps.iter().any(|s| &s.uuid == u));
        let last = config.last.filter(|u| !u.trim().is_empty());
        let mut rundown = Self {
            steps,
            last,
            next,
            to_come: config.to_come,
            ..Self::default()
        };
        if !rundown.last_is_removed() {
            rundown.to_come.clear();
        }
        rundown.set_fade_secs(config.fade_secs);
        rundown
    }
}

/// Steps with a UUID each of their own, tidied.
fn unique(steps: Vec<Step>) -> Vec<Step> {
    let mut seen = std::collections::HashSet::new();
    steps
        .into_iter()
        .filter(|s| !s.uuid.trim().is_empty() && seen.insert(s.uuid.clone()))
        .map(|mut step| {
            step.tidy();
            step
        })
        .collect()
}

/// A name for a new step, given the names there are: the first letter no
/// step is called by — "A", then "B" — as a running order is lettered; past
/// "Z", "Step 27".
pub fn new_name(names: &[&str]) -> String {
    ('A'..='Z')
        .map(String::from)
        .find(|letter| {
            !names
                .iter()
                .any(|name| name == letter || name.starts_with(&format!("{letter} ")))
        })
        .unwrap_or_else(|| format!("Step {}", names.len() + 1))
}

#[cfg(test)]
mod tests;
