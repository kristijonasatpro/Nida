//! Bus state and isolation tests.
//!
//! The isolation tests are the load-bearing ones: an MVP acceptance criterion
//! is that a command addressing one bus cannot switch another.

use super::*;

const EPS: f32 = 1e-6;

/// Monotonic frame-clock reading a take is started at.
const T0: f64 = 100.0;

fn bus() -> Bus {
    Bus::new("TEST", BusCanvas::default())
}

fn ch(uuid: &str) -> BusInputRef {
    BusInputRef::Channel(uuid.to_string())
}

/// Add a bus that is expected to be accepted.
fn push(bank: &mut BusBank, bus: Bus) -> String {
    bank.add(bus).expect("uuid should be unique")
}

// ── Construction ─────────────────────────────────────────────────────

#[test]
fn new_bus_is_black_on_both_pvw_and_pgm() {
    let bus = bus();
    assert_eq!(bus.preview(), &BusInputRef::Black);
    assert_eq!(bus.program(), &BusInputRef::Black);
    assert!(!bus.is_transitioning());
}

#[test]
fn new_bus_owns_an_audio_mix_matching_its_uuid() {
    let bus = bus();
    assert_eq!(bus.audio_mix().as_str(), bus.uuid());
}

#[test]
fn buses_get_distinct_uuids() {
    assert_ne!(bus().uuid(), bus().uuid());
}

// ── Preview / Program ────────────────────────────────────────────────

#[test]
fn set_preview_never_touches_program() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    assert_eq!(bus.preview(), &ch("cam1"));
    assert_eq!(
        bus.program(),
        &BusInputRef::Black,
        "PGM must not follow PVW"
    );
}

#[test]
fn cut_flip_flops_preview_and_program() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.cut();
    assert_eq!(bus.program(), &ch("cam1"));
    assert_eq!(
        bus.preview(),
        &BusInputRef::Black,
        "outgoing program should return to preview"
    );

    bus.set_preview(ch("slides"));
    bus.cut();
    assert_eq!(bus.program(), &ch("slides"));
    assert_eq!(bus.preview(), &ch("cam1"));
}

#[test]
fn cut_cancels_an_in_flight_transition() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(2.0), T0).expect("take should start");
    assert!(bus.is_transitioning());
    bus.cut();
    assert!(!bus.is_transitioning());
    assert_eq!(bus.program(), &ch("cam1"));
}

// ── AUTO ─────────────────────────────────────────────────────────────

#[test]
fn auto_starts_a_transition_from_program_to_preview() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take should start");

    let transition = bus.transition().expect("transition should be in flight");
    assert_eq!(transition.from, BusInputRef::Black);
    assert_eq!(transition.to, ch("cam1"));
    assert!(transition.progress().abs() < EPS);
}

#[test]
fn auto_does_not_settle_program_until_it_completes() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take should start");
    assert!(!bus.tick(T0 + 0.5));
    assert_eq!(
        bus.program(),
        &BusInputRef::Black,
        "program stays outgoing mid-transition"
    );
    assert!((bus.transition().unwrap().progress() - 0.5).abs() < EPS);
}

#[test]
fn completing_a_transition_flip_flops_like_a_cut() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take should start");
    assert!(!bus.tick(T0 + 0.5));
    assert!(bus.tick(T0 + 1.0), "tick reaching duration should complete");
    assert_eq!(bus.program(), &ch("cam1"));
    assert_eq!(bus.preview(), &BusInputRef::Black);
    assert!(!bus.is_transitioning());
}

#[test]
fn auto_uses_the_default_duration_when_none_given() {
    let mut bus = bus();
    bus.auto_duration = 4.0;
    bus.set_preview(ch("cam1"));
    bus.auto(None, T0).expect("take should start");
    assert!((bus.transition().unwrap().duration - 4.0).abs() < f64::from(EPS));
}

#[test]
fn a_non_positive_duration_is_rejected_not_downgraded_to_a_cut() {
    for duration in [0.0, -1.0] {
        let mut bus = bus();
        bus.set_preview(ch("cam1"));
        assert_eq!(
            bus.auto(Some(duration), T0),
            Err(AutoRejected::InvalidDuration),
            "duration {duration} should be refused"
        );
        assert!(!bus.is_transitioning());
        assert_eq!(
            bus.program(),
            &BusInputRef::Black,
            "a refused take must not put the source on air — CUT is its own command"
        );
    }
}

#[test]
fn a_non_finite_duration_is_rejected() {
    for duration in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut bus = bus();
        bus.set_preview(ch("cam1"));
        assert_eq!(
            bus.auto(Some(duration), T0),
            Err(AutoRejected::InvalidDuration)
        );
        assert_eq!(bus.program(), &BusInputRef::Black);
    }
}

#[test]
fn a_second_auto_during_a_take_is_refused_rather_than_restarting_it() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(2.0), T0).expect("first take starts");
    bus.tick(T0 + 1.0);
    let progress = bus.transition().unwrap().progress();

    bus.set_preview(ch("slides"));
    assert_eq!(
        bus.auto(Some(2.0), T0),
        Err(AutoRejected::AlreadyTransitioning),
        "restarting would snap the visible blend back to the outgoing source"
    );
    let live = bus.transition().expect("original take still running");
    assert_eq!(live.to, ch("cam1"), "the running take is unchanged");
    assert!(
        (live.progress() - progress).abs() < EPS,
        "the running take did not lose its progress"
    );
}

#[test]
fn a_clock_that_goes_backwards_does_not_corrupt_a_take() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take starts");
    // A reading earlier than the start must clamp to zero elapsed, not
    // produce a negative that later reads as progress.
    assert!(!bus.tick(T0 - 5.0));
    let t = bus.transition().expect("still running");
    assert!(t.elapsed >= 0.0, "elapsed went negative: {}", t.elapsed);
    assert!(t.progress() >= 0.0);
    assert!(bus.tick(T0 + 1.0), "it still completes normally");
}

#[test]
fn eased_mix_runs_from_zero_to_one() {
    let mut bus = bus();
    bus.easing = CrossfadeEasing::Linear;
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take should start");
    assert!(bus.transition().unwrap().mix().abs() < EPS);
    bus.tick(T0 + 0.25);
    assert!((bus.transition().unwrap().mix() - 0.25).abs() < EPS);
}

#[test]
fn tick_without_a_transition_is_a_no_op() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    assert!(!bus.tick(T0 + 1.0));
    assert_eq!(bus.program(), &BusInputRef::Black);
    assert_eq!(bus.preview(), &ch("cam1"));
}

// ── Isolation — the MVP acceptance criterion ─────────────────────────

#[test]
fn taking_a_source_on_one_bus_does_not_change_another() {
    let mut bank = BusBank::new();
    let stream = push(&mut bank, Bus::new("STREAM", BusCanvas::new(1920, 1080)));
    let led = push(&mut bank, Bus::new("LED", BusCanvas::new(1920, 480)));
    let tv = push(&mut bank, Bus::new("TV", BusCanvas::new(1920, 1080)));

    bank.get_mut(&led).unwrap().set_preview(ch("slides"));
    bank.get_mut(&led).unwrap().cut();

    assert_eq!(bank.get(&led).unwrap().program(), &ch("slides"));
    assert_eq!(
        bank.get(&stream).unwrap().program(),
        &BusInputRef::Black,
        "STREAM must not follow LED"
    );
    assert_eq!(
        bank.get(&tv).unwrap().program(),
        &BusInputRef::Black,
        "TV must not follow LED"
    );
}

#[test]
fn transitioning_one_bus_does_not_advance_another() {
    let mut bank = BusBank::new();
    let a = push(&mut bank, Bus::new("A", BusCanvas::default()));
    let b = push(&mut bank, Bus::new("B", BusCanvas::default()));

    bank.get_mut(&a).unwrap().set_preview(ch("cam1"));
    bank.get_mut(&a)
        .unwrap()
        .auto(Some(1.0), T0)
        .expect("take should start");

    let completed = bank.tick(T0 + 0.5);
    assert!(completed.is_empty());
    assert!(bank.get(&a).unwrap().is_transitioning());
    assert!(
        !bank.get(&b).unwrap().is_transitioning(),
        "B should have no transition of its own"
    );

    let completed = bank.tick(T0 + 1.0);
    assert_eq!(completed, vec![a.clone()], "only A should complete");
    assert_eq!(bank.get(&b).unwrap().program(), &BusInputRef::Black);
}

#[test]
fn buses_can_share_one_source_concurrently() {
    let mut bank = BusBank::new();
    let a = push(&mut bank, Bus::new("A", BusCanvas::default()));
    let b = push(&mut bank, Bus::new("B", BusCanvas::default()));

    for id in [&a, &b] {
        bank.get_mut(id).unwrap().set_preview(ch("cam1"));
        bank.get_mut(id).unwrap().cut();
    }

    assert_eq!(bank.get(&a).unwrap().program(), &ch("cam1"));
    assert_eq!(bank.get(&b).unwrap().program(), &ch("cam1"));
    assert_eq!(bank.referencing("cam1").count(), 2);
}

#[test]
fn each_bus_keeps_its_own_canvas() {
    let mut bank = BusBank::new();
    let led = push(&mut bank, Bus::new("LED", BusCanvas::new(1920, 480)));
    let tv = push(&mut bank, Bus::new("TV", BusCanvas::new(1920, 1080)));

    assert_eq!(bank.get(&led).unwrap().canvas().height, 480);
    assert_eq!(bank.get(&tv).unwrap().canvas().height, 1080);

    bank.get_mut(&led)
        .unwrap()
        .set_canvas(BusCanvas::new(1408, 768));
    assert_eq!(bank.get(&led).unwrap().canvas(), BusCanvas::new(1408, 768));
    assert_eq!(
        bank.get(&tv).unwrap().canvas(),
        BusCanvas::new(1920, 1080),
        "resizing LED must not resize TV"
    );
}

#[test]
fn each_bus_owns_a_distinct_audio_mix() {
    let mut bank = BusBank::new();
    let a = push(&mut bank, Bus::new("A", BusCanvas::default()));
    let b = push(&mut bank, Bus::new("B", BusCanvas::default()));
    assert_ne!(
        bank.get(&a).unwrap().audio_mix(),
        bank.get(&b).unwrap().audio_mix()
    );
}

// ── Bank addressing ──────────────────────────────────────────────────

#[test]
fn unknown_uuid_reaches_no_bus() {
    let mut bank = BusBank::with_legacy_default();
    assert!(bank.get("nosuchid").is_none());
    assert!(bank.get_mut("nosuchid").is_none());
    assert!(bank.remove("nosuchid").is_none());
}

#[test]
fn add_and_remove_round_trip() {
    let mut bank = BusBank::new();
    assert!(bank.is_empty());
    let id = push(&mut bank, Bus::new("A", BusCanvas::default()));
    assert_eq!(bank.len(), 1);
    let removed = bank.remove(&id).expect("bus should exist");
    assert_eq!(removed.uuid(), id);
    assert!(bank.is_empty());
}

#[test]
fn legacy_default_bank_has_one_1080p_bus() {
    let bank = BusBank::with_legacy_default();
    assert_eq!(bank.len(), 1);
    let bus = bank.iter().next().unwrap();
    assert_eq!(bus.name, "PGM");
    assert_eq!(bus.canvas(), BusCanvas::new(1920, 1080));
    // As the single program was: what Main shows, not black.
    assert_eq!(bus.program(), &BusInputRef::Master);
    assert_eq!(bus.preview(), &BusInputRef::Master);
}

#[test]
fn references_covers_preview_program_and_transition() {
    let mut bus = bus();
    assert!(!bus.references("cam1"));

    bus.set_preview(ch("cam1"));
    assert!(bus.references("cam1"), "preview counts");

    bus.cut();
    assert!(bus.references("cam1"), "program counts");

    bus.set_preview(ch("slides"));
    bus.auto(Some(1.0), T0).expect("take should start");
    assert!(
        bus.references("cam1"),
        "outgoing side of a transition counts"
    );
    assert!(bus.references("slides"), "incoming side counts");
}

// ── Value types ──────────────────────────────────────────────────────

#[test]
fn canvas_clamps_degenerate_dimensions() {
    let canvas = BusCanvas::new(0, 0);
    assert_eq!(canvas.width, 1);
    assert_eq!(canvas.height, 1);
}

#[test]
fn canvas_aspect_matches_dimensions() {
    assert!((BusCanvas::new(1920, 1080).aspect() - 16.0 / 9.0).abs() < EPS);
    assert!((BusCanvas::new(1920, 480).aspect() - 4.0).abs() < EPS);
}

#[test]
fn input_ref_exposes_its_uuid() {
    assert_eq!(BusInputRef::Black.uuid(), None);
    assert_eq!(ch("cam1").uuid(), Some("cam1"));
    assert_eq!(BusInputRef::Deck("d1".into()).uuid(), Some("d1"));
    assert!(ch("cam1").addresses("cam1"));
    assert!(!ch("cam1").addresses("cam2"));
    assert!(!BusInputRef::Black.addresses("cam1"));
}

#[test]
fn a_deck_and_a_channel_with_the_same_uuid_are_distinct_refs() {
    assert_ne!(ch("x"), BusInputRef::Deck("x".into()));
}

#[test]
fn input_ref_round_trips_through_json() {
    for input in [
        BusInputRef::Black,
        ch("cam1"),
        BusInputRef::Deck("deck1".into()),
    ] {
        let json = serde_json::to_string(&input).expect("serialize");
        let back: BusInputRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(input, back);
    }
}

#[test]
fn canvas_round_trips_through_json() {
    let canvas = BusCanvas::new(1920, 480);
    let json = serde_json::to_string(&canvas).expect("serialize");
    let back: BusCanvas = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(canvas, back);
}

#[test]
fn input_ref_displays_readably() {
    assert_eq!(BusInputRef::Black.to_string(), "Black");
    assert_eq!(ch("cam1").to_string(), "Ch cam1");
    assert_eq!(BusInputRef::Deck("d1".into()).to_string(), "Deck d1");
}

// ── Persistence ──────────────────────────────────────────────────────

fn known(channels: &[&str]) -> KnownSources {
    KnownSources {
        channels: channels.iter().map(|u| (*u).to_string()).collect(),
        decks: std::collections::BTreeSet::new(),
        master: true,
    }
}

fn known_decks(decks: &[&str]) -> KnownSources {
    KnownSources {
        channels: std::collections::BTreeSet::new(),
        decks: decks.iter().map(|u| (*u).to_string()).collect(),
        master: true,
    }
}

#[test]
fn a_bus_round_trips_through_its_config() {
    let mut original = Bus::new("LED", BusCanvas::new(1920, 480));
    original.set_preview(ch("cam1"));
    original.cut();
    original.set_preview(ch("slides"));
    original.auto_duration = 2.5;
    original.easing = CrossfadeEasing::EaseOut;

    let restored = Bus::from_config(original.to_config());
    assert_eq!(restored.uuid(), original.uuid());
    assert_eq!(restored.name, "LED");
    assert_eq!(restored.canvas(), BusCanvas::new(1920, 480));
    assert_eq!(restored.preview(), &ch("slides"));
    assert_eq!(restored.program(), &ch("cam1"));
    assert_eq!(restored.audio_mix(), original.audio_mix());
    assert!((restored.auto_duration - 2.5).abs() < EPS);
    assert_eq!(restored.easing, CrossfadeEasing::EaseOut);
}

#[test]
fn an_in_flight_transition_is_not_persisted() {
    let mut bus = Bus::new("STREAM", BusCanvas::default());
    bus.set_preview(ch("cam1"));
    bus.auto(Some(10.0), T0).expect("take should start");
    assert!(bus.is_transitioning());

    let restored = Bus::from_config(bus.to_config());
    assert!(
        !restored.is_transitioning(),
        "restoring mid-take would put indeterminate content on air"
    );
    assert_eq!(restored.program(), &BusInputRef::Black, "program unchanged");
    assert_eq!(restored.preview(), &ch("cam1"), "preview still staged");
}

#[test]
fn a_bank_round_trips_through_json() {
    let mut bank = BusBank::new();
    push(&mut bank, Bus::new("STREAM", BusCanvas::new(1920, 1080)));
    push(&mut bank, Bus::new("LED", BusCanvas::new(1920, 480)));

    let json = serde_json::to_string(&bank.to_configs()).expect("serialize");
    let configs: Vec<BusConfig> = serde_json::from_str(&json).expect("deserialize");
    let restored = BusBank::from_configs(configs);

    assert_eq!(restored.len(), 2);
    let names: Vec<&str> = restored.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["STREAM", "LED"], "order is preserved");
    assert_eq!(restored.iter().next().unwrap().canvas().height, 1080);
    assert_eq!(restored.iter().nth(1).unwrap().canvas().height, 480);
}

#[test]
fn a_config_missing_its_audio_mix_derives_one_from_the_uuid() {
    let json = r#"{"uuid":"abc12345","name":"LED"}"#;
    let config: BusConfig = serde_json::from_str(json).expect("deserialize legacy config");
    let bus = Bus::from_config(config);
    assert_eq!(bus.audio_mix().as_str(), "abc12345");
    assert_eq!(
        bus.canvas(),
        BusCanvas::default(),
        "canvas defaults to 1080p"
    );
    assert_eq!(bus.program(), &BusInputRef::Black);
    assert!((bus.auto_duration - DEFAULT_AUTO_DURATION).abs() < EPS);
}

#[test]
fn a_corrupt_auto_duration_falls_back_instead_of_failing_the_load() {
    // Parseable but nonsensical numbers fall back rather than failing the load.
    for bad in ["0.0", "-3.0"] {
        let json = format!(r#"{{"uuid":"a1","name":"X","auto_duration":{bad}}}"#);
        let config = serde_json::from_str::<BusConfig>(&json)
            .unwrap_or_else(|e| panic!("{bad} should still parse: {e}"));
        let bus = Bus::from_config(config);
        assert!(
            (bus.auto_duration - DEFAULT_AUTO_DURATION).abs() < EPS,
            "duration {bad} should fall back to the default"
        );
    }

    // A wrong *type* is a parse error, not a fallback. Asserted rather than
    // skipped, so the boundary between the two is pinned: the claim is that a
    // bad number cannot make a show unloadable, not that any garbage cannot.
    for wrong_type in ["null", r#""fast""#, "true"] {
        let json = format!(r#"{{"uuid":"a1","name":"X","auto_duration":{wrong_type}}}"#);
        assert!(
            serde_json::from_str::<BusConfig>(&json).is_err(),
            "{wrong_type} is a type error and must be reported as one"
        );
    }
}

#[test]
fn duplicate_uuids_are_dropped_on_load() {
    let configs: Vec<BusConfig> = serde_json::from_str(
        r#"[{"uuid":"dup","name":"A"},{"uuid":"dup","name":"B"},{"uuid":"other","name":"C"}]"#,
    )
    .expect("deserialize");
    let bank = BusBank::from_configs(configs);
    assert_eq!(
        bank.len(),
        2,
        "the second 'dup' must not create a second bus"
    );
    assert!(bank.get("dup").is_some());
    assert_eq!(bank.get("dup").unwrap().name, "A", "first wins");
    assert!(bank.get("other").is_some());
}

// ── Reconciliation — routing must never resolve to the wrong source ──

#[test]
fn routing_that_still_resolves_survives_reconciliation() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("cam1"));
    bank.get_mut(&id).unwrap().cut();

    let unresolved = bank.reconcile(&known(&["cam1", "cam2"]));
    assert!(unresolved.is_empty(), "nothing unresolved: {unresolved:?}");
    let bus = bank.get(&id).unwrap();
    assert_eq!(bus.effective_program(), &ch("cam1"));
    assert!(bus.program_resolved());
}

#[test]
fn an_unresolvable_source_goes_black_on_air_but_is_not_erased() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("gone"));
    bank.get_mut(&id).unwrap().cut();

    let unresolved = bank.reconcile(&known(&["cam1"]));
    assert_eq!(unresolved.len(), 1);
    let bus = bank.get(&id).unwrap();
    assert_eq!(
        bus.effective_program(),
        &BusInputRef::Black,
        "an unresolvable source must never resolve to a different source"
    );
    assert!(!bus.program_resolved());
    assert_eq!(
        bus.program(),
        &ch("gone"),
        "the configured route is retained so loading the right scene recovers it"
    );
}

#[test]
fn a_recovered_scene_restores_the_retained_route() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("cam1"));
    bank.get_mut(&id).unwrap().cut();

    bank.reconcile(&known(&["other"]));
    assert!(!bank.get(&id).unwrap().program_resolved());

    // The correct scene is loaded later.
    let unresolved = bank.reconcile(&known(&["cam1"]));
    assert!(unresolved.is_empty());
    let bus = bank.get(&id).unwrap();
    assert!(bus.program_resolved());
    assert_eq!(bus.effective_program(), &ch("cam1"), "routing came back");
}

#[test]
fn reconciliation_only_marks_the_unresolvable_side() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("cam1"));
    bank.get_mut(&id).unwrap().cut(); // PGM = cam1
    bank.get_mut(&id).unwrap().set_preview(ch("gone")); // PVW = gone

    assert_eq!(bank.reconcile(&known(&["cam1"])).len(), 1);
    let bus = bank.get(&id).unwrap();
    assert!(bus.program_resolved(), "the live side is untouched");
    assert_eq!(bus.effective_program(), &ch("cam1"));
    assert!(!bus.preview_resolved());
}

#[test]
fn a_channel_reference_naming_a_deck_does_not_resolve() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("TV", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("deck1"));

    // "deck1" exists — but as a deck, not a channel.
    let unresolved = bank.reconcile(&known_decks(&["deck1"]));
    assert_eq!(
        unresolved.len(),
        1,
        "a flat UUID set would wrongly accept this"
    );
    assert!(!bank.get(&id).unwrap().preview_resolved());
}

#[test]
fn a_deck_reference_resolves_against_decks() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("TV", BusCanvas::default()));
    bank.get_mut(&id)
        .unwrap()
        .set_preview(BusInputRef::Deck("deck1".into()));

    assert!(bank.reconcile(&known_decks(&["deck1"])).is_empty());
    assert!(bank.get(&id).unwrap().preview_resolved());
}

#[test]
fn one_buses_broken_routing_does_not_affect_another() {
    let mut bank = BusBank::new();
    let broken = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    let intact = push(&mut bank, Bus::new("TV", BusCanvas::default()));
    bank.get_mut(&broken).unwrap().set_preview(ch("gone"));
    bank.get_mut(&intact).unwrap().set_preview(ch("cam1"));

    bank.reconcile(&known(&["cam1"]));
    assert!(!bank.get(&broken).unwrap().preview_resolved());
    assert!(bank.get(&intact).unwrap().preview_resolved());
    assert_eq!(bank.get(&intact).unwrap().effective_preview(), &ch("cam1"));
}

// ── Regressions from external review ─────────────────────────────────

#[test]
fn staging_a_source_during_a_take_is_not_discarded_on_completion() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(1.0), T0).expect("take starts");
    // Operator lines up the next source while the take runs.
    bus.set_preview(ch("slides"));
    assert!(bus.tick(T0 + 1.0), "take completes");

    assert_eq!(bus.program(), &ch("cam1"), "the take landed");
    assert_eq!(
        bus.preview(),
        &ch("slides"),
        "the operator's staged source must survive completion"
    );
}

#[test]
fn an_untouched_preview_still_flip_flops_on_completion() {
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.cut(); // PGM = cam1
    bus.set_preview(ch("slides"));
    bus.auto(Some(1.0), T0).expect("take starts");
    assert!(bus.tick(T0 + 1.0));

    assert_eq!(bus.program(), &ch("slides"));
    assert_eq!(
        bus.preview(),
        &ch("cam1"),
        "with no operator input the outgoing source returns to preview"
    );
}

#[test]
fn a_long_take_keeps_sub_frame_resolution() {
    // Derived from a timestamp rather than accumulated, so a ten-minute take
    // still resolves a single frame near its end.
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(600.0), T0).expect("take starts");
    bus.tick(T0 + 599.0);
    let a = bus.transition().unwrap().progress();
    bus.tick(T0 + 599.0 + 1.0 / 60.0);
    let b = bus.transition().unwrap().progress();
    assert!(
        b > a,
        "progress advanced by one frame near the end: {a} -> {b}"
    );
}

#[test]
fn a_degenerate_canvas_in_a_stage_file_is_clamped_on_load() {
    // Derived deserialization would write the fields directly and bypass
    // BusCanvas::new, yielding a 0-sized GPU texture at stage 4.
    let canvas: BusCanvas = serde_json::from_str(r#"{"width":0,"height":0}"#).expect("parse");
    assert_eq!(canvas.width, 1);
    assert_eq!(canvas.height, 1);

    let config: BusConfig =
        serde_json::from_str(r#"{"uuid":"a1","name":"X","canvas":{"width":0,"height":480}}"#)
            .expect("parse config");
    assert_eq!(Bus::from_config(config).canvas(), BusCanvas::new(1, 480));
}

#[test]
fn adding_a_bus_whose_uuid_is_taken_is_refused() {
    let mut bank = BusBank::new();
    let first = Bus::new("A", BusCanvas::default());
    let uuid = first.uuid().to_string();
    push(&mut bank, first);

    let clash = Bus::from_config(BusConfig {
        uuid: uuid.clone(),
        name: "B".into(),
        canvas: BusCanvas::default(),
        preview: BusInputRef::Black,
        program: BusInputRef::Black,
        audio_mix: None,
        auto_duration: DEFAULT_AUTO_DURATION,
        easing: CrossfadeEasing::EaseInOut,
        hidden_layers: Vec::new(),
        hidden_sources: Vec::new(),
    });
    assert!(
        bank.add(clash).is_err(),
        "a duplicate id would make every bus-addressed command ambiguous"
    );
    assert_eq!(bank.len(), 1);
    assert_eq!(bank.get(&uuid).unwrap().name, "A");
}

#[test]
fn an_empty_bank_is_distinct_from_a_default_one() {
    assert_eq!(BusBank::empty().len(), 0);
    assert_eq!(BusBank::with_legacy_default().len(), 1);
    // An operator who deleted every bus must get an empty bank back.
    assert_eq!(BusBank::from_configs(Vec::new()).len(), 0);
}

// ── Second review round ──────────────────────────────────────────────

#[test]
fn a_take_started_at_the_frame_clock_does_not_complete_on_that_frame() {
    // A 10ms take must survive the frame it was started in, whichever side of
    // the tick the command landed on.
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.auto(Some(0.010), T0).expect("take starts");
    assert!(!bus.tick(T0), "no time has passed yet");
    assert!(bus.is_transitioning());
    assert!(bus.tick(T0 + 0.016), "it completes on the next frame");
}

#[test]
fn a_take_started_after_a_tick_is_not_charged_for_the_preceding_stall() {
    // Native UI commands run after the frame tick. With a start timestamp the
    // take is charged only from when it started, so a long stall before the
    // command cannot jump it.
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    // Frame N ticked at T0; a 30s stall happened before this command.
    let started_after_tick = T0 + 30.0;
    bus.auto(Some(5.0), started_after_tick)
        .expect("take starts");
    // Frame N+1, one frame later.
    assert!(
        !bus.tick(started_after_tick + 0.016),
        "the take must not be jumped by time that preceded it"
    );
    assert!(bus.is_transitioning());
}

#[test]
fn staging_away_and_back_during_a_take_still_counts_as_operator_input() {
    // Comparing preview by value cannot tell "untouched" from "changed away
    // and back", and would discard the operator's second explicit choice.
    let mut bus = bus();
    bus.set_preview(ch("cam1"));
    bus.cut(); // PGM = cam1
    bus.set_preview(ch("slides"));
    bus.auto(Some(1.0), T0).expect("take starts");

    bus.set_preview(ch("other"));
    bus.set_preview(ch("cam1")); // back to the same value as the outgoing PGM
    assert!(bus.tick(T0 + 1.0));

    assert_eq!(bus.program(), &ch("slides"));
    assert_eq!(
        bus.preview(),
        &ch("cam1"),
        "the operator's explicit re-stage must survive, not be treated as untouched"
    );
}

#[test]
fn taking_an_unresolved_preview_does_not_launder_it_into_a_resolved_program() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    bank.get_mut(&id).unwrap().set_preview(ch("gone"));
    bank.reconcile(&known(&["cam1"]));
    assert!(!bank.get(&id).unwrap().preview_resolved());

    let bus = bank.get_mut(&id).unwrap();
    bus.auto(Some(1.0), T0).expect("take starts");
    assert!(bus.tick(T0 + 1.0));

    assert!(
        !bus.program_resolved(),
        "an unresolved source must stay unresolved after being taken"
    );
    assert_eq!(bus.effective_program(), &BusInputRef::Black);
}

#[test]
fn reconciliation_refreshes_both_in_flight_endpoints() {
    // Both sides must be re-checked. An earlier version left PGM as Black,
    // which resolves trivially, so deleting the outgoing refresh entirely still
    // passed. Here the outgoing side is a real source that disappears and the
    // incoming side is one that survives, so each endpoint is mutation
    // sensitive on its own.
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::default()));
    {
        let bus = bank.get_mut(&id).unwrap();
        bus.set_preview(ch("cam1"));
        bus.cut(); // PGM = cam1
        bus.set_preview(ch("slides")); // PVW = slides
        bus.auto(Some(2.0), T0).expect("take starts");
    }

    // cam1 (outgoing) is gone; slides (incoming) survives.
    bank.reconcile(&known(&["slides"]));

    let bus = bank.get_mut(&id).unwrap();
    let take = bus.transition().expect("still in flight");
    assert!(
        !take.from_resolved,
        "the outgoing endpoint stopped resolving and must be marked"
    );
    assert!(
        take.to_resolved,
        "the incoming endpoint still resolves and must stay marked"
    );

    assert!(bus.tick(T0 + 2.0), "take completes");
    assert!(
        bus.program_resolved(),
        "it lands on the surviving source, so program resolves"
    );
    assert!(
        !bus.preview_resolved(),
        "and the vanished outgoing source flips back to preview unresolved"
    );
}

#[test]
fn a_structural_diff_restores_configuration_without_touching_what_is_on_air() {
    let mut bank = BusBank::new();
    let id = push(&mut bank, Bus::new("LED", BusCanvas::new(1920, 1080)));
    let before = bank.to_configs();

    // Operator takes a source and starts another take, then resizes.
    {
        let bus = bank.get_mut(&id).unwrap();
        bus.set_preview(ch("cam1"));
        bus.cut();
        bus.set_preview(ch("slides"));
        bus.auto(Some(10.0), T0).expect("take starts");
        bus.set_canvas(BusCanvas::new(1408, 768));
    }

    // Undo the resize.
    bank.apply_structural_diff(&before);

    let bus = bank.get(&id).unwrap();
    assert_eq!(
        bus.canvas(),
        BusCanvas::new(1920, 1080),
        "the resize was undone"
    );
    assert_eq!(
        bus.program(),
        &ch("cam1"),
        "undoing an edit must not change what is on air"
    );
    assert!(
        bus.is_transitioning(),
        "undoing an edit must not cancel a take in flight"
    );
}

#[test]
fn a_structural_diff_adds_and_removes_buses() {
    let mut bank = BusBank::new();
    let kept = push(&mut bank, Bus::new("KEEP", BusCanvas::default()));
    let snapshot = bank.to_configs();

    let added = push(&mut bank, Bus::new("ADDED", BusCanvas::default()));
    bank.apply_structural_diff(&snapshot);
    assert!(bank.get(&added).is_none(), "undoing an add removes the bus");
    assert!(bank.get(&kept).is_some());

    // Redo-style: a bus present only in the target is recreated.
    let mut sparse = BusBank::empty();
    sparse.apply_structural_diff(&snapshot);
    assert_eq!(sparse.len(), 1);
    assert_eq!(sparse.get(&kept).unwrap().name, "KEEP");
}

// ── Fourth review round ──────────────────────────────────────────────

#[test]
fn two_buses_cannot_load_with_the_same_effective_audio_mix() {
    // `{uuid:"m", audio_mix:null}` derives its mix from the uuid, so it
    // collides with `{uuid:"b", audio_mix:"m"}` even though neither states
    // "m" twice. Independent bus audio depends on these being distinct.
    let configs: Vec<BusConfig> = serde_json::from_str(
        r#"[{"uuid":"m","name":"A"},{"uuid":"b","name":"B","audio_mix":"m"}]"#,
    )
    .expect("parse");
    let (bank, rejected) = BusBank::from_configs_reporting(configs);
    assert_eq!(bank.len(), 1, "the colliding bus must not load");
    assert_eq!(bank.iter().next().unwrap().name, "A", "first identity wins");
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].contains("audio mix"),
        "the reason should name the collision: {}",
        rejected[0]
    );
}

#[test]
fn an_empty_bus_uuid_is_rejected_at_the_load_boundary() {
    let configs: Vec<BusConfig> =
        serde_json::from_str(r#"[{"uuid":"","name":"NOBODY"},{"uuid":"ok","name":"FINE"}]"#)
            .expect("parse");
    let (bank, rejected) = BusBank::from_configs_reporting(configs);
    assert_eq!(bank.len(), 1, "an unaddressable bus must not load");
    assert!(bank.get("ok").is_some());
    assert_eq!(rejected.len(), 1);
}

#[test]
fn distinct_audio_mixes_load_normally() {
    let configs: Vec<BusConfig> = serde_json::from_str(
        r#"[{"uuid":"a","name":"A"},{"uuid":"b","name":"B"},{"uuid":"c","name":"C","audio_mix":"own"}]"#,
    )
    .expect("parse");
    let (bank, rejected) = BusBank::from_configs_reporting(configs);
    assert_eq!(bank.len(), 3);
    assert!(rejected.is_empty(), "nothing to reject: {rejected:?}");
}

#[test]
fn a_bus_rejected_for_a_duplicate_uuid_does_not_reserve_its_audio_mix() {
    // Identities must be committed atomically. If the rejected bus reserved
    // mix "m", the later valid bus that legitimately owns it would be refused.
    let configs: Vec<BusConfig> = serde_json::from_str(
        r#"[
            {"uuid":"dup","name":"A","audio_mix":"a"},
            {"uuid":"dup","name":"B","audio_mix":"m"},
            {"uuid":"c","name":"C","audio_mix":"m"}
        ]"#,
    )
    .expect("parse");
    let (bank, rejected) = BusBank::from_configs_reporting(configs);

    assert_eq!(
        bank.len(),
        2,
        "A and C load; only the duplicate-uuid B is refused"
    );
    assert!(bank.get("dup").is_some());
    assert!(
        bank.get("c").is_some(),
        "C owns mix 'm' legitimately and must not be blocked by the rejected B"
    );
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].contains("duplicates uuid"), "{}", rejected[0]);
}

// ── Resource release policy ──────────────────────────────────────────
//
// These pin `HeldResources::minus`. The policy differs by manager and getting
// it uniform in either direction is a bug: suppressing a reference-counted
// release leaves a capture thread running forever, while not suppressing a
// shared one freezes an on-air deck.

#[cfg(test)]
mod release_policy {
    use crate::app::HeldSet;

    /// A reference-counted device is released once per retiring holder, even
    /// when a survivor holds the same device.
    #[test]
    fn refcounted_devices_are_never_suppressed() {
        let held = crate::app::HeldResources::for_test(Some(7), vec![3], Some(11), None, Some(5));
        let survivor =
            crate::app::HeldResources::for_test(Some(7), vec![3], Some(11), None, Some(5));
        let keep = crate::app::HeldResources::union(std::slice::from_ref(&survivor));
        let releasing = held.minus(&keep);

        assert_eq!(
            releasing.camera_for_test(),
            Some(7),
            "a camera held by a survivor must still be released once for this holder"
        );
        assert_eq!(releasing.depth_for_test(), vec![3], "depth likewise");
        assert_eq!(
            releasing.screen_capture_for_test(),
            Some(11),
            "screen capture is reference counted too"
        );
        assert_eq!(
            releasing.ndi_for_test(),
            Some(5),
            "NDI receivers are per-deck, so nothing to suppress"
        );
    }

    /// A stream receiver shared by URL is suppressed while any survivor holds
    /// it, and released once nobody does.
    #[test]
    fn a_shared_stream_receiver_is_only_released_by_its_last_holder() {
        let held = crate::app::HeldResources::for_test(None, vec![], None, Some(2), None);

        let survivor = crate::app::HeldResources::for_test(None, vec![], None, Some(2), None);
        let keep = crate::app::HeldResources::union(std::slice::from_ref(&survivor));
        assert_eq!(
            held.clone_for_test().minus(&keep).srt_for_test(),
            None,
            "another deck is on air from this receiver — stopping it would freeze them"
        );

        let none_left = HeldSet::default();
        assert_eq!(
            held.minus(&none_left).srt_for_test(),
            Some(2),
            "the last holder releases it"
        );
    }
}

// ── Region model ─────────────────────────────────────────────────────
//
// A bus declares which rectangle of the master raster is its picture. Nothing
// composites at a second resolution; a destination samples that rectangle.

#[test]
fn a_full_size_region_samples_the_whole_master() {
    let (scale, offset) = BusCanvas::new(1920, 1080).uv(1920, 1080);
    assert!((scale[0] - 1.0).abs() < EPS && (scale[1] - 1.0).abs() < EPS);
    assert!(
        offset[0].abs() < EPS && offset[1].abs() < EPS,
        "at the origin"
    );
}

#[test]
fn a_band_samples_its_share_of_the_master() {
    // 1920x480 of a 1920x1080 master: full width, 4/9 of the height. Those are
    // the master's own pixels, so the crop is lossless.
    let (scale, offset) = BusCanvas::new(1920, 480).uv(1920, 1080);
    assert!((scale[0] - 1.0).abs() < EPS, "full width");
    assert!((scale[1] - 480.0 / 1080.0).abs() < EPS, "4/9 height");
    assert!(
        offset[0].abs() < EPS && offset[1].abs() < EPS,
        "at the origin"
    );
}

#[test]
fn a_placed_region_offsets_into_the_master() {
    let (_, offset) = BusCanvas::new(1920, 480).at(0, 300).uv(1920, 1080);
    assert!((offset[1] - 300.0 / 1080.0).abs() < EPS, "moved down");
}

#[test]
fn a_region_hanging_off_the_edge_is_clamped_rather_than_sampling_outside() {
    // The master can shrink under a saved show. Sampling past the edge would
    // read whatever the sampler's address mode gives — smeared or repeated edge
    // pixels, on air, with nothing to say why.
    let (scale, offset) = BusCanvas::new(1920, 480).at(0, 900).uv(1920, 1080);
    assert!(
        (offset[1] + scale[1] - 1.0).abs() < EPS,
        "pushed back inside"
    );
    assert!(offset[1] >= 0.0);
}

#[test]
fn a_region_larger_than_the_master_is_clamped_to_it() {
    let (scale, offset) = BusCanvas::new(3840, 2160).uv(1920, 1080);
    assert!((scale[0] - 1.0).abs() < EPS && (scale[1] - 1.0).abs() < EPS);
    assert!(
        offset[0].abs() < EPS && offset[1].abs() < EPS,
        "at the origin"
    );
}

#[test]
fn fits_within_reports_whether_clamping_would_happen() {
    assert!(BusCanvas::new(1920, 480).at(0, 300).fits_within(1920, 1080));
    assert!(!BusCanvas::new(1920, 480).at(0, 900).fits_within(1920, 1080));
    assert!(!BusCanvas::new(3840, 2160).fits_within(1920, 1080));
}

#[test]
fn fits_within_does_not_wrap_on_a_huge_origin() {
    // `u32::MAX + 1536` wraps to 1535 in release, which would read as fitting.
    assert!(!BusCanvas::new(1536, 432)
        .at(u32::MAX, 0)
        .fits_within(1920, 1080));
    assert!(!BusCanvas::new(1536, 432)
        .at(0, u32::MAX)
        .fits_within(1920, 1080));
}

#[test]
fn a_region_survives_the_stage_file_with_its_placement() {
    // Through JSON, as the stage file stores it — an in-memory config round
    // trip never exercises `BusCanvas`'s hand-written Deserialize.
    let bus = Bus::new("LED", BusCanvas::new(1536, 432).at(192, 40));
    let json = serde_json::to_string(&bus.to_config()).expect("serialize");
    let config: BusConfig = serde_json::from_str(&json).expect("parse");
    assert_eq!(
        Bus::from_config(config).canvas(),
        BusCanvas::new(1536, 432).at(192, 40)
    );
}

#[test]
fn a_region_survives_persistence_with_its_placement() {
    let bus = Bus::new("LED", BusCanvas::new(1920, 480).at(0, 300));
    let restored = Bus::from_config(bus.to_config());
    assert_eq!(restored.canvas(), BusCanvas::new(1920, 480).at(0, 300));
}

#[test]
fn a_config_without_an_origin_loads_at_the_master_origin() {
    // Stage files written before regions had a position.
    let config: BusConfig =
        serde_json::from_str(r#"{"uuid":"a1","name":"LED","canvas":{"width":1920,"height":480}}"#)
            .expect("parse");
    assert_eq!(Bus::from_config(config).canvas(), BusCanvas::new(1920, 480));
}

#[test]
fn a_full_frame_region_stays_full_frame_across_a_resize() {
    let full = BusCanvas::new(1920, 1080);
    assert_eq!(
        full.rescaled((1920, 1080), (3840, 2160)),
        BusCanvas::new(3840, 2160)
    );
    assert_eq!(
        full.rescaled((1920, 1080), (1280, 720)),
        BusCanvas::new(1280, 720)
    );
    // Even into a different shape: each axis follows its own.
    assert_eq!(
        full.rescaled((1920, 1080), (1080, 1920)),
        BusCanvas::new(1080, 1920)
    );
}

#[test]
fn a_strip_stays_the_same_strip_across_a_resize() {
    let strip = BusCanvas::new(1536, 432).at(192, 0);
    assert_eq!(
        strip.rescaled((1920, 1080), (3840, 2160)),
        BusCanvas::new(3072, 864).at(384, 0)
    );
    assert_eq!(
        strip.rescaled((1920, 1080), (2560, 1440)),
        BusCanvas::new(2048, 576).at(256, 0)
    );
    assert_eq!(strip.rescaled((1920, 1080), (1920, 1080)), strip);
}

#[test]
fn a_rescaled_region_stays_inside_the_master_and_non_empty() {
    let corner = BusCanvas::new(3, 3).at(1917, 1077);
    let small = corner.rescaled((1920, 1080), (64, 36));
    assert!(small.width >= 1 && small.height >= 1);
    assert!(small.fits_within(64, 36), "{small:?}");
}

#[test]
fn a_bus_added_to_a_quarantined_bank_is_held_black_too() {
    let mut bank = BusBank::new();
    bank.quarantine_routing();
    let uuid = push(&mut bank, Bus::new_on_main("TV", BusCanvas::default()));
    let bus = bank.get(&uuid).unwrap();
    assert_eq!(bus.effective_program(), &BusInputRef::Black);
    assert_eq!(bus.effective_preview(), &BusInputRef::Black);
    bank.reconcile_trusted(&known(&[]));
    assert_eq!(
        bank.get(&uuid).unwrap().effective_program(),
        &BusInputRef::Master,
        "its route is kept, for a load that lifts the quarantine"
    );
}

#[test]
fn master_resolves_in_any_scene_but_not_in_quarantine() {
    assert!(
        known(&[]).resolves(&BusInputRef::Master),
        "the master mix exists in every scene, so a bus on Master never reads missing"
    );
    assert!(
        !KnownSources::EMPTY.resolves(&BusInputRef::Master),
        "quarantined routing holds every route black, Master included"
    );

    let mut bank = BusBank::new();
    let mut bus = Bus::new("REC", BusCanvas::default());
    bus.set_preview(BusInputRef::Master);
    bus.cut();
    let uuid = push(&mut bank, bus);
    bank.quarantine_routing();
    bank.reconcile(&known(&[]));
    assert_eq!(
        bank.get(&uuid).unwrap().effective_program(),
        &BusInputRef::Black
    );
    bank.reconcile_trusted(&known(&[]));
    assert_eq!(
        bank.get(&uuid).unwrap().effective_program(),
        &BusInputRef::Master
    );

    let json = serde_json::to_string(&BusInputRef::Master).expect("serialize");
    assert_eq!(json, "\"Master\"");
    let parsed: BusInputRef = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, BusInputRef::Master);
    assert_eq!(BusInputRef::Master.uuid(), None);
}

#[test]
fn hidden_layers_toggle_and_survive_a_save() {
    let mut bus = Bus::new("REC", BusCanvas::default());
    assert!(bus.set_layer_hidden("ovl00001", true));
    assert!(
        !bus.set_layer_hidden("ovl00001", true),
        "hiding a hidden layer changes nothing"
    );
    assert_eq!(bus.hidden_layers(), ["ovl00001".to_string()]);

    let json = serde_json::to_string(&bus.to_config()).expect("serialize");
    let restored = Bus::from_config(serde_json::from_str(&json).expect("deserialize"));
    assert_eq!(restored.hidden_layers(), ["ovl00001".to_string()]);

    assert!(bus.set_layer_hidden("ovl00001", false));
    assert!(bus.hidden_layers().is_empty());
    let json = serde_json::to_string(&bus.to_config()).expect("serialize");
    assert!(
        !json.contains("hidden_layers"),
        "a bus hiding nothing writes no field, so older builds read the file unchanged: {json}"
    );
}
