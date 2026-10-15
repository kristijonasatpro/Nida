//! Several buses taken at once, in step.
//!
//! A wall in three parts is three screens on three buses. Taken one by one,
//! the parts change one after another; a show wants them to change as one
//! picture does (an operator, 2026-10-13: "i need to fade or cut one by one
//! but during a show with resolume they fade all regions at once").
//!
//! [`BusBank::take_together`] starts every take from one clock reading, with
//! one duration and one curve, whatever each bus's own `AUTO` settings are —
//! so the dissolves are the same frame for frame and end on the same frame.

use super::{default_easing, Bus, BusBank, BusInputRef, KnownSources};

/// How buses taken together get to their inputs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Together {
    /// At once.
    Cut,
    /// A dissolve over this many seconds, the same for every bus.
    Fade(f32),
}

/// What [`BusBank::take_together`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TakenTogether {
    /// The buses taken, by UUID. One already settled on its input is not
    /// among them: nothing was done to it.
    pub taken: Vec<String>,
    /// Seconds until every bus has landed: the fade time when buses were
    /// started; what is left of their takes when they were all on their way
    /// already; none after a cut.
    pub settle_secs: f64,
}

/// Why [`BusBank::take_together`] took nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TogetherRejected {
    /// Bus routing is held black until a workspace loads whole (see
    /// [`BusBank::quarantine_routing`]).
    Held,
    /// The fade time was not finite and positive.
    InvalidDuration,
    /// What cannot be shown, each as a phrase: a bus that is gone, or a bus,
    /// by name, whose input is not in the show. Taking the rest would leave
    /// a picture nobody asked for.
    Missing(Vec<String>),
    /// These buses, by name, are in the middle of a take. A dissolve has two
    /// ends, so one heading elsewhere cannot turn towards a third; and one
    /// already heading the right way started earlier, so the others starting
    /// now would not land with it.
    Fading(Vec<String>),
}

impl std::fmt::Display for TogetherRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => write!(
                f,
                "bus routing is held black until a workspace loads with matching stage and scene"
            ),
            Self::InvalidDuration => write!(f, "the fade time must be finite and positive"),
            Self::Missing(what) => write!(f, "not in the show: {}", what.join(", ")),
            Self::Fading(buses) => write!(f, "still fading: {}", buses.join(", ")),
        }
    }
}

impl Bus {
    /// Where the bus is going: what its running take ends on, else its
    /// Program — and whether that resolves against the loaded scene.
    pub fn heading(&self) -> (&BusInputRef, bool) {
        self.transition.as_ref().map_or(
            (&self.program, self.program_resolved),
            |t| (&t.to, t.to_resolved),
        )
    }
}

impl BusBank {
    /// Take each of `sets` — a bus, by UUID, and what it is to show — at one
    /// instant: all cut on this frame, or all dissolve over one time on one
    /// curve from `now`. All of them, or none.
    ///
    /// A bus settled on its input already is left alone. A cut lands every
    /// other, one mid-take included. A bus named twice is taken to its first
    /// input.
    ///
    /// # Errors
    ///
    /// Nothing is changed when it fails.
    /// - [`TogetherRejected::Missing`] when a bus is gone, or `known` does
    ///   not have its input.
    /// - [`TogetherRejected::Fading`] when a fade is asked for and a bus is
    ///   mid-take — unless every bus is already where it should be or on its
    ///   way there, when there is nothing to do. A cut is never refused for
    ///   this: a cut ends a take.
    /// - [`TogetherRejected::Held`] while routing is quarantined, unless
    ///   every input is black.
    /// - [`TogetherRejected::InvalidDuration`] for a fade time that is not
    ///   finite and positive.
    pub fn take_together(
        &mut self,
        sets: &[(String, BusInputRef)],
        how: Together,
        now: f64,
        known: &KnownSources,
    ) -> Result<TakenTogether, TogetherRejected> {
        let plan: Vec<(usize, BusInputRef)> = self
            .plan_together(sets, how, known)?
            .into_iter()
            .map(|(idx, input)| (idx, input.clone()))
            .collect();
        let mut taken = Vec::with_capacity(plan.len());
        for (idx, input) in plan {
            let bus = &mut self.buses[idx];
            bus.set_preview(input);
            match how {
                Together::Cut => bus.cut(),
                Together::Fade(over) => bus.start_take(f64::from(over), default_easing(), now),
            }
            taken.push(bus.uuid().to_string());
        }
        // What is left of the takes still running on these buses: the fade
        // just started, or — nothing started — the ones they were on already.
        let settle_secs = sets
            .iter()
            .filter_map(|(uuid, _)| self.get(uuid)?.transition())
            .map(|t| (t.duration - (now - t.started_at).max(0.0)).max(0.0))
            .fold(0.0, f64::max);
        Ok(TakenTogether { taken, settle_secs })
    }

    /// Whether [`Self::take_together`] would take `sets` now, changing
    /// nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::take_together`].
    pub fn check_together(
        &self,
        sets: &[(String, BusInputRef)],
        how: Together,
        known: &KnownSources,
    ) -> Result<(), TogetherRejected> {
        self.plan_together(sets, how, known).map(|_| ())
    }

    /// The buses a take of `sets` has to act on, by index, with their inputs.
    fn plan_together<'a>(
        &self,
        sets: &'a [(String, BusInputRef)],
        how: Together,
        known: &KnownSources,
    ) -> Result<Vec<(usize, &'a BusInputRef)>, TogetherRejected> {
        if let Together::Fade(over) = how {
            if !over.is_finite() || over <= 0.0 {
                return Err(TogetherRejected::InvalidDuration);
            }
        }
        if self.routing_quarantined && sets.iter().any(|(_, input)| *input != BusInputRef::Black) {
            return Err(TogetherRejected::Held);
        }
        let mut seen: Vec<&str> = Vec::new();
        let mut missing = Vec::new();
        // Buses to start, and those mid-take: on their way to their input,
        // and on their way elsewhere.
        let mut plan = Vec::new();
        let (mut arriving, mut elsewhere) = (Vec::new(), Vec::new());
        for (uuid, input) in sets {
            if seen.contains(&uuid.as_str()) {
                continue;
            }
            seen.push(uuid);
            let Some(idx) = self.buses.iter().position(|b| b.uuid() == uuid) else {
                missing.push("a bus that is gone".to_string());
                continue;
            };
            let bus = &self.buses[idx];
            if !known.resolves(input) {
                missing.push(format!("{}'s input", bus.name));
                continue;
            }
            let there = bus.heading() == (input, true);
            match (bus.transition.is_some(), there) {
                (false, true) => {}
                (false, false) => plan.push((idx, input)),
                (true, _) if how == Together::Cut => plan.push((idx, input)),
                (true, true) => arriving.push(bus.name.clone()),
                (true, false) => elsewhere.push(bus.name.clone()),
            }
        }
        if !missing.is_empty() {
            return Err(TogetherRejected::Missing(missing));
        }
        if !elsewhere.is_empty() || (!arriving.is_empty() && !plan.is_empty()) {
            elsewhere.extend(arriving);
            return Err(TogetherRejected::Fading(elsewhere));
        }
        Ok(plan)
    }

    /// Whether every one of `sets` is what its bus shows, or is on its way
    /// to: the picture [`Self::take_together`] leaves. Nothing set is not
    /// shown.
    pub fn shows_together(&self, sets: &[(String, BusInputRef)]) -> bool {
        !sets.is_empty()
            && sets.iter().all(|(uuid, input)| {
                self.get(uuid)
                    .is_some_and(|bus| bus.heading() == (input, true))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{BusCanvas, BusTransition, CrossfadeEasing};
    use super::*;

    const T0: f64 = 100.0;

    fn ch(uuid: &str) -> BusInputRef {
        BusInputRef::Channel(uuid.to_string())
    }

    fn known(channels: &[&str]) -> KnownSources {
        KnownSources {
            channels: channels.iter().map(|c| (*c).to_string()).collect(),
            master: true,
            ..KnownSources::default()
        }
    }

    fn all() -> KnownSources {
        known(&["intro", "slides", "logo", "camera"])
    }

    /// Three buses on "intro", each with an `AUTO` time and curve of its own.
    fn wall() -> (BusBank, [String; 3]) {
        let mut bank = BusBank::new();
        let mut ids = Vec::new();
        for (name, secs, easing) in [
            ("Left", 0.5, CrossfadeEasing::Linear),
            ("Middle", 3.0, CrossfadeEasing::EaseInOut),
            ("Right", 5.0, CrossfadeEasing::Linear),
        ] {
            let mut bus = Bus::new(name, BusCanvas::default());
            bus.auto_duration = secs;
            bus.easing = easing;
            bus.set_preview(ch("intro"));
            bus.cut();
            ids.push(bank.add(bus).expect("a new bus"));
        }
        (bank, ids.try_into().expect("three"))
    }

    fn sets(ids: &[String; 3], inputs: [&str; 3]) -> Vec<(String, BusInputRef)> {
        ids.iter()
            .zip(inputs)
            .map(|(id, input)| (id.clone(), ch(input)))
            .collect()
    }

    fn mixes(bank: &BusBank) -> Vec<Option<f32>> {
        bank.iter()
            .map(|b| b.transition().map(BusTransition::mix))
            .collect()
    }

    /// Every bus as it stands, to tell that nothing was touched.
    fn state(bank: &BusBank) -> Vec<String> {
        bank.iter().map(|b| format!("{b:?}")).collect()
    }

    /// The parts of a wall fade as one picture: the same blend on every bus
    /// on every frame, whatever each bus's own AUTO time and curve, and all
    /// done on the same frame.
    #[test]
    fn buses_taken_together_fade_in_step_and_land_together() {
        let (mut bank, ids) = wall();
        let want = sets(&ids, ["slides", "logo", "slides"]);
        assert_eq!(bank.check_together(&want, Together::Fade(2.0), &all()), Ok(()));
        let taken = bank
            .take_together(&want, Together::Fade(2.0), T0, &all())
            .expect("taken");
        assert_eq!(taken.taken, ids.to_vec());
        assert_eq!(taken.settle_secs, 2.0, "landed when the fade is done");

        for at in [0.0, 0.3, 0.7, 1.0, 1.6, 1.99] {
            assert!(bank.tick(T0 + at).is_empty(), "none done at {at}");
            let mixes = mixes(&bank);
            let first = mixes[0].expect("fading");
            assert!(
                mixes.iter().all(|m| *m == Some(first)),
                "at {at} s the parts are apart: {mixes:?}"
            );
            for bus in &bank {
                assert_eq!(bus.program(), &ch("intro"), "still leaving at {at}");
            }
        }
        // One curve, not each bus's own: a quarter of the way through, an
        // ease has moved less than a straight line.
        bank.tick(T0 + 0.5);
        let quarter = mixes(&bank)[0].expect("fading");
        assert!(quarter < 0.2, "eased, not linear: {quarter}");
        assert!(bank.shows_together(&want), "on its way counts as shown");

        let mut landed = bank.tick(T0 + 2.0);
        landed.sort();
        let mut expected = ids.to_vec();
        expected.sort();
        assert_eq!(landed, expected, "every part lands on the same frame");
        let shown: Vec<_> = bank.iter().map(|b| b.program().clone()).collect();
        assert_eq!(shown, [ch("slides"), ch("logo"), ch("slides")]);
        // As after any take: what was on air waits on Preview.
        assert!(bank.iter().all(|b| b.preview() == &ch("intro")));
        // The buses' own AUTO settings are as they were.
        let own: Vec<_> = bank.iter().map(|b| (b.auto_duration, b.easing)).collect();
        assert_eq!(own[0], (0.5, CrossfadeEasing::Linear));
        assert_eq!(own[1], (3.0, CrossfadeEasing::EaseInOut));
    }

    /// A part settled on what the picture wants is not faded to itself, and
    /// what the operator staged on it stays staged.
    #[test]
    fn a_bus_settled_on_its_input_is_left_alone() {
        let (mut bank, ids) = wall();
        bank.get_mut(&ids[1]).unwrap().set_preview(ch("camera"));
        let want = sets(&ids, ["slides", "intro", "slides"]);
        for (how, at) in [(Together::Fade(1.0), T0), (Together::Cut, T0 + 5.0)] {
            let taken = bank.take_together(&want, how, at, &all()).expect("taken");
            let middle = bank.get(&ids[1]).unwrap();
            assert!(!taken.taken.contains(&ids[1]) && !middle.is_transitioning());
            let settle = if how == Together::Cut { 0.0 } else { 1.0 };
            assert_eq!(taken.settle_secs, settle);
            assert_eq!(middle.preview(), &ch("camera"));
            assert!(bank.shows_together(&want));
        }
        assert!(bank.iter().all(|b| !b.is_transitioning()), "the cut landed the two fading");
    }

    /// Taken again while its fade runs, the same picture is nothing to do —
    /// not refused, and not started over.
    #[test]
    fn the_same_fade_asked_for_again_changes_nothing() {
        let (mut bank, ids) = wall();
        let want = sets(&ids, ["slides", "logo", "intro"]);
        bank.take_together(&want, Together::Fade(2.0), T0, &all())
            .expect("taken");
        bank.tick(T0 + 0.5);
        let before = state(&bank);
        let again = bank
            .take_together(&want, Together::Fade(9.0), T0 + 0.5, &all())
            .expect("nothing to do is not a refusal");
        assert!(again.taken.is_empty());
        // The picture lands when the fade already running does: a second and
        // a half from now, whatever fade time was asked for this time.
        assert_eq!(again.settle_secs, 1.5);
        assert_eq!(state(&bank), before);
    }

    /// With every bus on its way already, by takes of their own, the picture
    /// has landed when the last of them has — and a bus that is not part of
    /// the picture does not come into it.
    #[test]
    fn the_picture_has_landed_when_the_last_of_its_buses_has() {
        let (mut bank, ids) = wall();
        for (bus, input, secs) in [(0, "slides", 4.0), (1, "logo", 1.0), (2, "camera", 9.0)] {
            let bus = bank.get_mut(&ids[bus]).unwrap();
            bus.set_preview(ch(input));
            bus.auto(Some(secs), T0).expect("its own take");
        }
        bank.tick(T0 + 0.5);
        let two = vec![(ids[1].clone(), ch("logo")), (ids[0].clone(), ch("slides"))];
        let taken = bank
            .take_together(&two, Together::Fade(2.0), T0 + 0.5, &all())
            .expect("nothing to do");
        assert!(taken.taken.is_empty());
        assert_eq!(taken.settle_secs, 3.5, "Left's, the longer — not Right's 8.5");
    }

    /// A fade one bus cannot join is not started on the others: the parts
    /// would not land together. A bus heading elsewhere cannot join; nor can
    /// one that set off for the right input earlier.
    #[test]
    fn a_fade_is_refused_whole_while_a_bus_is_mid_take() {
        let (mut bank, ids) = wall();
        let right = bank.get_mut(&ids[2]).unwrap();
        right.set_preview(ch("camera"));
        right.auto(Some(4.0), T0).expect("its own take");
        bank.tick(T0 + 1.0);
        let before = state(&bank);

        // Heading elsewhere.
        let elsewhere = sets(&ids, ["slides", "logo", "slides"]);
        let refused = Err(TogetherRejected::Fading(vec!["Right".into()]));
        assert_eq!(bank.check_together(&elsewhere, Together::Fade(1.0), &all()), refused);
        assert_eq!(
            bank.take_together(&elsewhere, Together::Fade(1.0), T0 + 1.0, &all()),
            Err(TogetherRejected::Fading(vec!["Right".into()]))
        );
        assert_eq!(state(&bank), before, "nothing changed on any bus");

        // Heading the right way, but the others would start a second later.
        let same_way = sets(&ids, ["slides", "logo", "camera"]);
        assert_eq!(
            bank.take_together(&same_way, Together::Fade(1.0), T0 + 1.0, &all()),
            Err(TogetherRejected::Fading(vec!["Right".into()]))
        );
        assert_eq!(state(&bank), before, "nothing changed on any bus");

        // Its settled Program is the input wanted, but it is leaving it.
        let leaving = sets(&ids, ["intro", "intro", "intro"]);
        assert_eq!(
            bank.take_together(&leaving, Together::Fade(1.0), T0 + 1.0, &all()),
            Err(TogetherRejected::Fading(vec!["Right".into()]))
        );
        assert!(!bank.shows_together(&leaving));

        // Its take done, the same fade goes.
        bank.tick(T0 + 4.0);
        let taken = bank
            .take_together(&elsewhere, Together::Fade(1.0), T0 + 4.0, &all())
            .expect("taken");
        assert_eq!(taken.taken, ids.to_vec());
    }

    /// A cut ends whatever take is running: every part shows its input on
    /// this frame — one on its way there, one on its way elsewhere, and one
    /// leaving the very input the cut wants.
    #[test]
    fn a_cut_lands_every_bus_at_once_even_mid_take() {
        let (mut bank, ids) = wall();
        let left = bank.get_mut(&ids[0]).unwrap();
        left.set_preview(ch("slides"));
        left.auto(Some(4.0), T0).expect("its own take");
        let middle = bank.get_mut(&ids[1]).unwrap();
        middle.set_preview(ch("camera"));
        middle.auto(Some(4.0), T0).expect("its own take");
        let right = bank.get_mut(&ids[2]).unwrap();
        right.set_preview(ch("camera"));
        right.auto(Some(4.0), T0).expect("its own take");
        bank.tick(T0 + 1.0);

        let taken = bank
            .take_together(
                &sets(&ids, ["slides", "logo", "intro"]),
                Together::Cut,
                T0 + 1.0,
                &all(),
            )
            .expect("a cut is not refused");
        assert_eq!((taken.taken, taken.settle_secs), (ids.to_vec(), 0.0));
        assert!(bank.iter().all(|b| !b.is_transitioning()));
        let shown: Vec<_> = bank.iter().map(|b| b.program().clone()).collect();
        assert_eq!(shown, [ch("slides"), ch("logo"), ch("intro")]);
    }

    /// With an input no longer in the show, or a bus gone, nothing is taken:
    /// two thirds of a picture is not a picture.
    #[test]
    fn something_not_in_the_show_refuses_the_whole_take() {
        let (mut bank, ids) = wall();
        let without_logo = known(&["intro", "slides"]);
        let before = state(&bank);
        let want = sets(&ids, ["slides", "logo", "slides"]);
        for how in [Together::Fade(1.0), Together::Cut] {
            assert_eq!(
                bank.take_together(&want, how, T0, &without_logo),
                Err(TogetherRejected::Missing(vec!["Middle's input".into()]))
            );
            assert_eq!(state(&bank), before);
        }
        assert!(!bank.shows_together(&want));

        let mut gone = want.clone();
        gone.push(("nosuchbus".to_string(), BusInputRef::Black));
        assert_eq!(
            bank.take_together(&gone, Together::Cut, T0, &all()),
            Err(TogetherRejected::Missing(vec!["a bus that is gone".into()]))
        );
        assert_eq!(state(&bank), before);
        // A deck is not a channel that happens to share its UUID.
        let deck = vec![(ids[0].clone(), BusInputRef::Deck("slides".into()))];
        assert!(matches!(
            bank.take_together(&deck, Together::Cut, T0, &all()),
            Err(TogetherRejected::Missing(_))
        ));
    }

    /// Held black after a load that could not be trusted, the buses are not
    /// re-routed together either — but may all be sent to black.
    #[test]
    fn held_routing_refuses_all_but_black() {
        let (mut bank, ids) = wall();
        bank.quarantine_routing();
        let refused = bank.take_together(
            &sets(&ids, ["slides", "slides", "slides"]),
            Together::Cut,
            T0,
            &all(),
        );
        assert_eq!(refused, Err(TogetherRejected::Held));
        assert!(bank.iter().all(|b| b.program() == &ch("intro")));

        let black: Vec<_> = ids.iter().map(|id| (id.clone(), BusInputRef::Black)).collect();
        let taken = bank
            .take_together(&black, Together::Cut, T0, &all())
            .expect("black is always allowed");
        assert_eq!(taken.taken.len(), 3);
    }

    #[test]
    fn a_fade_time_that_is_not_a_time_is_refused() {
        let (mut bank, ids) = wall();
        for secs in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let refused = bank.take_together(
                &sets(&ids, ["slides", "slides", "slides"]),
                Together::Fade(secs),
                T0,
                &all(),
            );
            assert_eq!(refused, Err(TogetherRejected::InvalidDuration), "{secs}");
        }
        assert!(bank.iter().all(|b| !b.is_transitioning()));
    }

    /// A bus named twice goes to the first input named.
    #[test]
    fn a_bus_named_twice_is_taken_once() {
        let (mut bank, ids) = wall();
        let twice = vec![(ids[0].clone(), ch("slides")), (ids[0].clone(), ch("logo"))];
        let taken = bank
            .take_together(&twice, Together::Fade(1.0), T0, &all())
            .expect("taken");
        assert_eq!(taken.taken, [ids[0].clone()]);
        assert_eq!(bank.get(&ids[0]).unwrap().transition().unwrap().to, ch("slides"));
    }
}
