//! Main, the vMix way: one channel or input on air at a time — taken with a
//! cut or a fade — and overlay channels (a logo, a lower third) on top of
//! whatever is on it.
//!
//! The alternative to the layered VJ mix, where every channel shows at its
//! fader and a crossfader blends two. Which one the master is made from is
//! the scene's [`MixMode`]; in [`MixMode::Program`] the channel faders and
//! crossfader are not used, and a channel's opacity and blend mode matter
//! only when it is an overlay.

use std::collections::HashMap;

/// How the master is made.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MixMode {
    /// Every channel layered at its fader, a crossfader between two: the VJ
    /// mixer.
    #[default]
    Layered,
    /// One channel or input on Main, overlays on top: the vMix way.
    Program,
}

/// What is on Main.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum MainSource {
    /// Nothing: black.
    #[default]
    Black,
    /// A channel, every deck in it layered, by channel UUID.
    Channel(String),
    /// One deck on its own, by deck UUID.
    Deck(String),
}

impl MainSource {
    /// The UUID it names, if any.
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Self::Black => None,
            Self::Channel(u) | Self::Deck(u) => Some(u),
        }
    }
}

/// How a take that is not a cut changes Main — vMix's transitions.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MainTransition {
    /// The new picture dissolves in over the old.
    #[default]
    Fade,
    /// The layers both have — the same camera, clip or input — move and
    /// resize from where they are to where they will be; the rest fade.
    Merge,
    /// The new picture wipes across from the left.
    Wipe,
    /// The new picture slides in from the right, pushing the old out.
    Slide,
    /// The old picture zooms in and away, the new one behind it.
    Zoom,
    /// A circle opens from the middle onto the new picture.
    Iris,
}

impl MainTransition {
    pub const ALL: [Self; 6] = [
        Self::Fade,
        Self::Merge,
        Self::Wipe,
        Self::Slide,
        Self::Zoom,
        Self::Iris,
    ];

    /// Its name, as the take button says it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Fade => "Fade",
            Self::Merge => "Merge",
            Self::Wipe => "Wipe",
            Self::Slide => "Slide",
            Self::Zoom => "Zoom",
            Self::Iris => "Iris",
        }
    }

    /// How far along it shows at `progress` (0–1) of its time: a fade
    /// evenly; the moving ones easing in and out — gathering speed, then
    /// settling — as vMix's do.
    pub fn eased(self, progress: f32) -> f32 {
        let t = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 1.0 };
        match self {
            Self::Fade => t,
            _ if t < 0.5 => 4.0 * t * t * t,
            _ => 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0,
        }
    }
}

/// A fade in progress: `from` giving way to the program.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MainFade {
    pub from: MainSource,
    /// 0 at the start, 1 when the program has fully replaced `from`.
    pub progress: f32,
    /// Seconds the whole fade takes.
    pub duration: f32,
    /// How it changes Main.
    pub transition: MainTransition,
}

impl MainFade {
    /// How far along it shows (see [`MainTransition::eased`]).
    pub fn shown(&self) -> f32 {
        self.transition.eased(self.progress)
    }
}

/// Main as the state and the UI see it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct MainSnapshot {
    pub mode: MixMode,
    pub program: MainSource,
    /// The fade running, if any.
    pub fade: Option<MainFade>,
    pub fade_secs: f32,
    /// What a take that is not a cut does.
    pub transition: MainTransition,
    /// Overlay channels switched on, by UUID.
    pub overlays_on: Vec<String>,
}

impl MainSnapshot {
    pub fn of(mode: MixMode, main: &MainProgram) -> Self {
        Self {
            mode,
            program: main.program.clone(),
            fade: main.fade.clone(),
            fade_secs: main.fade_secs,
            transition: main.transition,
            overlays_on: main.overlays_on(),
        }
    }
}

/// An overlay channel's level: 0 off, 1 fully on, moving towards `target`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Level {
    now: f32,
    target: f32,
    /// Level change per second while moving.
    rate: f32,
    /// Set moving since the last tick (see [`MainProgram::tick`]).
    fresh: bool,
}

/// Default fade time, in seconds.
pub const DEFAULT_FADE_SECS: f32 = 1.0;

/// Main's state: what is on air, any fade running, and the overlays.
#[derive(Debug, Clone)]
pub struct MainProgram {
    pub program: MainSource,
    pub fade: Option<MainFade>,
    /// Seconds a fade takes (vMix's AUTO time).
    pub fade_secs: f32,
    /// What a take that is not a cut does.
    pub transition: MainTransition,
    overlays: HashMap<String, Level>,
    /// The fade was started since the last tick (see [`Self::tick`]).
    fade_fresh: bool,
}

impl Default for MainProgram {
    fn default() -> Self {
        Self {
            program: MainSource::Black,
            fade: None,
            fade_secs: DEFAULT_FADE_SECS,
            transition: MainTransition::Fade,
            overlays: HashMap::new(),
            fade_fresh: false,
        }
    }
}

impl MainProgram {
    /// Put `source` on Main: at once, or by [`Self::transition`] over
    /// [`Self::fade_secs`]. Taking what is already on air does nothing;
    /// taking during a fade starts from what the fade was heading to.
    pub fn take(&mut self, source: MainSource, fade: bool) {
        if source == self.program {
            if !fade {
                self.fade = None;
            }
            return;
        }
        let from = std::mem::replace(&mut self.program, source);
        self.fade = (fade && self.fade_secs > 0.0).then_some(MainFade {
            from,
            progress: 0.0,
            duration: self.fade_secs,
            transition: self.transition,
        });
        self.fade_fresh = self.fade.is_some();
    }

    /// Switch an overlay channel on or off: at once, or fading.
    pub fn set_overlay(&mut self, channel_uuid: &str, on: bool, fade: bool) {
        let target = if on { 1.0 } else { 0.0 };
        let secs = if fade { self.fade_secs } else { 0.0 };
        let level = self
            .overlays
            .entry(channel_uuid.to_string())
            .or_insert(Level {
                now: 0.0,
                target: 0.0,
                rate: 0.0,
                fresh: false,
            });
        // Only a fade that begins here is new to the next tick: told again
        // what it is doing already — on, every frame, by something that
        // keeps saying so — it must go on fading, not start over each time.
        let turning = (level.target - target).abs() > f32::EPSILON;
        level.target = target;
        if secs <= 0.0 {
            level.now = target;
            level.rate = 0.0;
            level.fresh = false;
        } else {
            level.rate = 1.0 / secs;
            level.fresh |= turning;
        }
    }

    /// Seconds until an overlay has faded on or off; none when it is where
    /// it is going.
    pub fn overlay_settle_secs(&self, channel_uuid: &str) -> f32 {
        self.overlays
            .get(channel_uuid)
            .filter(|l| l.rate > 0.0)
            .map_or(0.0, |l| (l.target - l.now).abs() / l.rate)
    }

    /// Whether an overlay is on (or on its way on).
    pub fn overlay_on(&self, channel_uuid: &str) -> bool {
        self.overlays
            .get(channel_uuid)
            .is_some_and(|l| l.target > 0.5)
    }

    /// How much of an overlay shows now, 0–1.
    pub fn overlay_level(&self, channel_uuid: &str) -> f32 {
        self.overlays.get(channel_uuid).map_or(0.0, |l| l.now)
    }

    /// The overlays switched on, by channel UUID, sorted.
    pub fn overlays_on(&self) -> Vec<String> {
        let mut on: Vec<String> = self
            .overlays
            .iter()
            .filter(|(_, l)| l.target > 0.5)
            .map(|(u, _)| u.clone())
            .collect();
        on.sort();
        on
    }

    /// Put saved overlays back on, as a scene loads.
    pub fn restore_overlays(&mut self, on: &[String]) {
        self.overlays.clear();
        for uuid in on {
            self.set_overlay(uuid, true, false);
        }
    }

    /// Whether `source` is on air: the program, or what a fade is leaving.
    pub fn shows(&self, source: &MainSource) -> bool {
        *source == self.program || self.fade.as_ref().is_some_and(|f| f.from == *source)
    }

    /// Move fades on by `dt` seconds: the time since the tick before.
    ///
    /// A fade started since that tick is not moved on by this one. `dt` is
    /// time that passed before it began — a whole frame of it, or, after a
    /// frame that stalled, half a second: counted, the fade's first picture
    /// was already part-way through, and after a stall it jumped. It starts
    /// from its start on the first frame drawn, as a bus's take does (see
    /// `BusTransition::started_at`).
    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let fresh = std::mem::take(&mut self.fade_fresh);
        if let Some(fade) = self.fade.as_mut().filter(|_| !fresh) {
            fade.progress += dt / fade.duration.max(1e-3);
            if fade.progress >= 1.0 {
                self.fade = None;
            }
        }
        self.overlays.retain(|_, l| {
            if std::mem::take(&mut l.fresh) {
                return true;
            }
            if l.now < l.target {
                l.now = (l.now + l.rate * dt).min(l.target);
            } else if l.now > l.target {
                l.now = (l.now - l.rate * dt).max(l.target);
            }
            // An overlay faded fully out is forgotten.
            l.now > 0.0 || l.target > 0.0
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(u: &str) -> MainSource {
        MainSource::Channel(u.into())
    }

    #[test]
    fn a_cut_replaces_the_program_at_once() {
        let mut m = MainProgram::default();
        m.take(ch("a"), false);
        assert_eq!(m.program, ch("a"));
        assert!(m.fade.is_none());
        assert!(m.shows(&ch("a")) && !m.shows(&MainSource::Black));
    }

    #[test]
    fn a_fade_shows_both_until_it_is_done() {
        let mut m = MainProgram::default();
        m.take(ch("a"), false);
        m.take(ch("b"), true);
        assert!(m.shows(&ch("a")) && m.shows(&ch("b")));
        m.tick(0.0);
        m.tick(0.5);
        let p = m.fade.as_ref().unwrap().progress;
        assert!(
            (p - 0.5).abs() < 1e-6,
            "halfway through a one-second fade: {p}"
        );
        m.tick(0.6);
        assert!(m.fade.is_none(), "done");
        assert!(!m.shows(&ch("a")) && m.shows(&ch("b")));
    }

    /// A take that is not a cut goes by the transition chosen when it
    /// starts; the moving ones ease in and out, a fade goes evenly.
    #[test]
    fn a_take_goes_by_the_transition_chosen() {
        let mut m = MainProgram::default();
        m.take(ch("a"), false);
        m.transition = MainTransition::Merge;
        m.take(ch("b"), true);
        m.transition = MainTransition::Wipe;
        let fade = m.fade.as_ref().expect("a transition");
        assert_eq!(fade.transition, MainTransition::Merge, "as when it started");
        let merge = MainTransition::Merge;
        assert!(merge.eased(0.1) < 0.1, "gathering speed");
        assert!((merge.eased(0.5) - 0.5).abs() < 1e-6);
        assert!(merge.eased(0.9) > 0.9, "settling");
        assert!((merge.eased(1.0) - 1.0).abs() < 1e-6 && merge.eased(0.0).abs() < 1e-6);
        assert!((MainTransition::Fade.eased(0.3) - 0.3).abs() < 1e-6, "a fade evenly");
    }

    /// A fade starts from its start on the first frame drawn after it is
    /// taken, however long the frame before took: the time since the tick
    /// before the take is not the fade's. An overlay fading on or off, the
    /// same. From then on every tick counts.
    #[test]
    fn a_fade_starts_from_its_start_however_long_the_frame_before_took() {
        let mut m = MainProgram::default();
        m.take(ch("a"), false);
        m.tick(0.016);
        m.take(ch("b"), true);
        m.set_overlay("logo", true, true);
        // The frame the take landed in stalled for half a second.
        m.tick(0.5);
        assert_eq!(m.fade.as_ref().map(|f| f.progress), Some(0.0));
        assert!(m.overlay_level("logo").abs() < 1e-6);
        m.tick(0.25);
        assert!((m.fade.as_ref().unwrap().progress - 0.25).abs() < 1e-6);
        assert!((m.overlay_level("logo") - 0.25).abs() < 1e-6);
        // A fade started over a running one starts from its start too, and
        // an overlay turned round mid-fade.
        m.take(ch("c"), true);
        m.set_overlay("logo", false, true);
        m.tick(0.5);
        assert_eq!(m.fade.as_ref().map(|f| f.progress), Some(0.0));
        assert!((m.overlay_level("logo") - 0.25).abs() < 1e-6);
        m.tick(0.1);
        assert!((m.fade.as_ref().unwrap().progress - 0.1).abs() < 1e-6);
        assert!((m.overlay_level("logo") - 0.15).abs() < 1e-6);
        // Told every frame what it is doing already, an overlay goes on
        // fading: only a fade that begins is new to the next tick.
        for _ in 0..3 {
            m.set_overlay("logo", false, true);
            m.tick(0.05);
        }
        assert!(m.overlay_level("logo").abs() < 1e-6, "{}", m.overlay_level("logo"));
        m.set_overlay("logo", true, true);
        m.tick(0.0);
        for _ in 0..4 {
            m.set_overlay("logo", true, true);
            m.tick(0.25);
        }
        assert!((m.overlay_level("logo") - 1.0).abs() < 1e-6, "on, said four times over");
        m.tick(1.0);
        // How long an overlay has to go: its fade's time at its start, what
        // is left of it on the way, none when it is there.
        assert!(m.overlay_settle_secs("logo").abs() < 1e-6);
        m.fade_secs = 2.0;
        m.set_overlay("logo", false, true);
        assert!((m.overlay_settle_secs("logo") - 2.0).abs() < 1e-6);
        m.tick(0.0);
        m.tick(0.5);
        assert!((m.overlay_settle_secs("logo") - 1.5).abs() < 1e-6);
        m.set_overlay("logo", true, true);
        assert!((m.overlay_settle_secs("logo") - 0.5).abs() < 1e-6, "back the way it came");
        m.set_overlay("logo", true, false);
        assert!(m.overlay_settle_secs("logo").abs() < 1e-6);
        assert!(m.overlay_settle_secs("no such overlay").abs() < 1e-6);
        m.fade_secs = DEFAULT_FADE_SECS;
        // A cut is at once, as ever.
        m.take(ch("a"), false);
        m.set_overlay("logo", true, false);
        assert!(m.fade.is_none() && (m.overlay_level("logo") - 1.0).abs() < 1e-6);
        m.tick(0.5);
        assert!((m.overlay_level("logo") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn taking_what_is_on_air_again_does_nothing() {
        let mut m = MainProgram::default();
        m.take(ch("a"), false);
        m.take(ch("a"), true);
        assert!(m.fade.is_none());
    }

    #[test]
    fn overlays_fade_in_and_out_and_are_forgotten_when_gone() {
        let mut m = MainProgram::default();
        m.set_overlay("logo", true, true);
        assert!(m.overlay_on("logo"));
        assert!(m.overlay_level("logo") < 1e-6);
        m.tick(0.0);
        m.tick(0.25);
        assert!((m.overlay_level("logo") - 0.25).abs() < 1e-6);
        m.tick(1.0);
        assert!((m.overlay_level("logo") - 1.0).abs() < 1e-6);
        m.set_overlay("logo", false, false);
        assert!(!m.overlay_on("logo"));
        assert!(
            m.overlay_level("logo").abs() < 1e-6,
            "a cut takes it off at once"
        );
        m.tick(0.1);
        assert!(m.overlays_on().is_empty());
    }

    #[test]
    fn saved_overlays_come_back_on() {
        let mut m = MainProgram::default();
        m.restore_overlays(&["b".into(), "a".into()]);
        assert_eq!(m.overlays_on(), ["a", "b"]);
        assert!((m.overlay_level("a") - 1.0).abs() < 1e-6);
    }
}
