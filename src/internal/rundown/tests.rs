//! The rundown's bookkeeping: what comes next, the wait, and what steps let
//! go of.

use super::*;

const T0: f64 = 50.0;

fn ch(uuid: &str) -> BusInputRef {
    BusInputRef::Channel(uuid.to_string())
}

/// A, B, C and D, each setting nothing.
fn abcd() -> (Rundown, [String; 4]) {
    let mut rundown = Rundown::default();
    let ids: Vec<String> = ["A", "B", "C", "D"]
        .into_iter()
        .map(|name| rundown.add(Step::new(name)))
        .collect();
    (rundown, ids.try_into().expect("four"))
}

/// A rundown as saved with these steps and nothing else known of it: what
/// undo puts back when the show was not on a removed step.
fn saved(steps: &[Step]) -> RundownConfig {
    RundownConfig {
        steps: steps.to_vec(),
        fade_secs: DEFAULT_FADE_SECS,
        last: None,
        next: None,
        to_come: Vec::new(),
    }
}

fn next(rundown: &Rundown) -> Option<&str> {
    rundown.next().map(|s| s.name.as_str())
}

fn last(rundown: &Rundown) -> Option<&str> {
    rundown
        .last()
        .and_then(|uuid| rundown.step(uuid))
        .map(|s| s.name.as_str())
}

fn names(rundown: &Rundown) -> Vec<&str> {
    rundown.steps().iter().map(|s| s.name.as_str()).collect()
}

/// GO walks the list from the top, and at the end takes nothing — never the
/// first step again.
#[test]
fn next_is_the_step_after_the_one_taken_and_none_at_the_end() {
    let (mut rundown, ids) = abcd();
    assert_eq!(next(&rundown), Some("A"));
    for (taken, then) in [(0, Some("B")), (1, Some("C")), (2, Some("D")), (3, None)] {
        assert!(rundown.taken(&ids[taken], true, T0, 0.0));
        assert_eq!(rundown.last(), Some(ids[taken].as_str()));
        assert_eq!(next(&rundown), then);
    }
    // A step taken out of order: the show goes on from it.
    rundown.taken(&ids[1], true, T0, 0.0);
    assert_eq!(next(&rundown), Some("C"));
    assert!(!rundown.taken("nosuchid", true, T0, 0.0));
    assert_eq!((last(&rundown), next(&rundown)), (Some("B"), Some("C")));
}

/// The operator can say which step GO takes next without taking anything:
/// the step taken last stays where the show is, and the next take puts GO
/// back in order.
#[test]
fn the_operator_can_say_which_step_go_takes_next() {
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[2], true, T0, 0.0);
    assert!(rundown.set_next(&ids[0]));
    assert_eq!((last(&rundown), next(&rundown)), (Some("C"), Some("A")));
    // At the end of the rundown, the top can be made next again.
    rundown.taken(&ids[3], true, T0, 0.0);
    assert_eq!(next(&rundown), None);
    assert!(rundown.set_next(&ids[0]));
    assert_eq!((last(&rundown), next(&rundown)), (Some("D"), Some("A")));
    // Taken, the order is the list's again.
    rundown.taken(&ids[0], true, T0, 0.0);
    assert_eq!((last(&rundown), next(&rundown)), (Some("A"), Some("B")));
    assert!(!rundown.set_next("nosuchid"));
    assert_eq!(next(&rundown), Some("B"));
    // The step said to be next, removed: GO goes by the list.
    rundown.set_next(&ids[3]);
    rundown.remove(&ids[3]);
    assert_eq!(next(&rundown), Some("B"));
}

/// Removing the step the show is on leaves GO where it was: on the step that
/// followed it. No step is marked as the one taken last any more.
#[test]
fn removing_a_step_keeps_go_on_the_step_that_followed() {
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[1], true, T0, 0.0);
    assert_eq!(next(&rundown), Some("C"));
    assert_eq!(rundown.remove(&ids[1]).map(|s| s.name), Some("B".into()));
    assert_eq!(names(&rundown), ["A", "C", "D"]);
    assert_eq!((last(&rundown), next(&rundown)), (None, Some("C")));
    // And the step that followed removed too: the one after that.
    rundown.remove(&ids[2]);
    assert_eq!((last(&rundown), next(&rundown)), (None, Some("D")));
    // And the step before, which stood in for it: still what followed.
    rundown.remove(&ids[0]);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["D"], Some("D")));

    // The first step, taken and removed: the top again.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[0], true, T0, 0.0);
    rundown.remove(&ids[0]);
    assert_eq!((rundown.last(), next(&rundown)), (None, Some("B")));
    // The last step, taken and removed: still the end, not the top.
    rundown.taken(&ids[3], true, T0, 0.0);
    rundown.remove(&ids[3]);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["B", "C"], None));
    // A step that is not where the show is: nothing moves.
    rundown.taken(&ids[1], true, T0, 0.0);
    rundown.remove(&ids[2]);
    assert_eq!((last(&rundown), next(&rundown)), (Some("B"), None));
    assert!(rundown.remove("nosuchid").is_none());
}

/// The step the show is on, removed and put back by undo, is the step the
/// show is on again: GO takes what follows it, not the step itself a second
/// time — wherever in the list it stood.
#[test]
fn a_step_removed_and_put_back_is_not_taken_twice() {
    for at in 0..4 {
        let (mut rundown, ids) = abcd();
        let before = rundown.steps().to_vec();
        rundown.taken(&ids[at], true, T0, 0.0);
        let (on, then) = (last(&rundown).map(String::from), next(&rundown).map(String::from));
        rundown.remove(&ids[at]);
        assert_eq!(last(&rundown), None, "the step taken last is gone");
        assert_eq!(next(&rundown).map(String::from), then, "GO goes on as before");
        rundown.restore_steps(&saved(&before));
        assert_eq!(
            (last(&rundown).map(String::from), next(&rundown).map(String::from)),
            (on, then),
            "step {at} back, the show is where it was"
        );
    }
    // Saved while the step is removed, there is no undo after a restart: no
    // step is the one taken last, and GO still takes what followed — or, the
    // removed step having been the last, nothing.
    for (at, then) in [(1, Some("C")), (3, None)] {
        let (mut rundown, ids) = abcd();
        rundown.taken(&ids[at], true, T0, 0.0);
        rundown.remove(&ids[at]);
        let text = serde_json::to_string(&rundown.to_config()).expect("written");
        let mut opened = Rundown::from_config(serde_json::from_str(&text).expect("read"));
        assert_eq!((last(&opened), next(&opened)), (None, then), "step {at}");
        // And what followed that, with the step that followed removed too.
        if at == 1 {
            opened.remove(&ids[2]);
            assert_eq!(next(&opened), Some("D"), "D is still to come after a restart");
        }
    }
}

/// A step made while the step the show is on is removed is still to come
/// after its making is undone and redone — with the removal of the step
/// taken last undone and redone before it, too.
#[test]
fn undo_and_redo_keep_what_was_made_as_still_to_come() {
    // As the app's history does: the rundown saved before each edit, put
    // back by undo, and the edit's result put back by redo.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    let before_remove = rundown.to_config();
    rundown.remove(&ids[3]);
    let before_add = rundown.to_config();
    rundown.add(Step::new("E"));
    let after_add = rundown.to_config();
    assert_eq!(next(&rundown), Some("E"));

    rundown.restore_steps(&before_add);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B", "C"], None));
    rundown.restore_steps(&before_remove);
    assert_eq!((last(&rundown), next(&rundown)), (Some("D"), None));
    rundown.restore_steps(&before_add);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B", "C"], None));
    rundown.restore_steps(&after_add);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B", "C", "E"], Some("E")));

    // A copy made to stand in for a step: both edits undone, both redone,
    // and the original removed — the copy is what GO takes.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[1], true, T0, 0.0);
    let before_remove = rundown.to_config();
    rundown.remove(&ids[1]);
    let before_copy = rundown.to_config();
    rundown.duplicate(&ids[2]).expect("a copy of C");
    let after_copy = rundown.to_config();
    rundown.restore_steps(&before_copy);
    rundown.restore_steps(&before_remove);
    rundown.restore_steps(&before_copy);
    rundown.restore_steps(&after_copy);
    rundown.remove(&ids[2]);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "C copy", "D"], Some("C copy")));

    // The show taken on to another step since the edits were made: what
    // they knew of what was to come is of another moment, and is not used.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    rundown.remove(&ids[3]);
    rundown.add(Step::new("E"));
    let with_e = rundown.to_config();
    rundown.taken(&ids[0], true, T0, 0.0);
    rundown.restore_steps(&with_e);
    assert_eq!((last(&rundown), next(&rundown)), (Some("A"), Some("B")), "by the list");
    // Nor when the step the show has since been on is removed in its turn:
    // E was to come after D, not after F, which was made and taken later.
    let f = rundown.add(Step::new("F"));
    rundown.taken(&f, true, T0, 0.0);
    rundown.remove(&f);
    assert_eq!(next(&rundown), None, "F was the last step");
    rundown.restore_steps(&with_e);
    assert_eq!((last(&rundown), next(&rundown)), (None, None), "still past the end");
}

/// With the step the show is on removed, a step made since is still to come:
/// one added at the end, and a copy of a step that is itself still to come.
#[test]
fn a_step_made_while_the_one_taken_last_is_removed_is_still_to_come() {
    // The show at its end, the last step removed, another added: GO takes it.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    rundown.remove(&ids[3]);
    assert_eq!(next(&rundown), None);
    rundown.add(Step::new("E"));
    assert_eq!(next(&rundown), Some("E"));

    // A copy made to take a step's place: the original removed, the copy is
    // what GO takes — not the step after it.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[1], true, T0, 0.0);
    rundown.remove(&ids[1]);
    rundown.duplicate(&ids[2]).expect("a copy of C");
    rundown.remove(&ids[2]);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "C copy", "D"], Some("C copy")));
    // A copy of a step the show has passed is not what GO takes.
    rundown.duplicate(&ids[0]).expect("a copy of A");
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "A copy", "C copy", "D"], Some("C copy")));

    // With the step taken last in place, the list alone says what is next.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    rundown.add(Step::new("E"));
    assert_eq!(next(&rundown), Some("E"));
    let text = serde_json::to_string(&rundown.to_config()).expect("written");
    assert!(!text.contains("to_come"), "nothing to write of it: {text}");
}

/// With the step the show is on removed, the steps before it taken away and
/// put back by undo and redo never send GO backwards: it goes on from the
/// nearest of them that is there, as they stood.
#[test]
fn go_never_goes_back_while_the_step_taken_last_is_removed() {
    // At the end of the show: D taken and removed, then C removed, and C
    // put back. GO stays at the end — it does not take C again.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    let with_d = rundown.steps().to_vec();
    rundown.remove(&ids[3]);
    let without_d = rundown.steps().to_vec();
    rundown.remove(&ids[2]);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B"], None));
    rundown.restore_steps(&saved(&without_d));
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B", "C"], None));
    rundown.restore_steps(&saved(&with_d));
    assert_eq!((last(&rundown), next(&rundown)), (Some("D"), None));

    // Steps added one by one, the second taken; both undone, the first
    // redone: the show was past A, and GO does not take it.
    let mut rundown = Rundown::default();
    let empty = rundown.steps().to_vec();
    let a = rundown.add(Step::new("A"));
    let only_a = rundown.steps().to_vec();
    let b = rundown.add(Step::new("B"));
    rundown.taken(&b, true, T0, 0.0);
    rundown.restore_steps(&saved(&only_a));
    assert_eq!((last(&rundown), next(&rundown)), (None, None), "past A, at the end");
    rundown.restore_steps(&saved(&empty));
    assert_eq!(next(&rundown), None, "nothing to take");
    rundown.restore_steps(&saved(&only_a));
    assert_eq!((last(&rundown), next(&rundown)), (None, None), "A is behind the show");
    // A taken again by hand, the show is simply on A.
    rundown.taken(&a, true, T0, 0.0);
    assert_eq!((last(&rundown), next(&rundown)), (Some("A"), None));

    // Steps moved while the step taken last is removed: at the end of the
    // show, a step the show has passed is not what GO takes, wherever it is
    // moved to.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[3], true, T0, 0.0);
    rundown.remove(&ids[3]);
    rundown.move_step(&ids[2], false);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "C", "B"], None));
    // In the middle of it, GO takes the first of what was still to come, as
    // the list now stands.
    let (mut rundown, ids) = abcd();
    rundown.taken(&ids[1], true, T0, 0.0);
    rundown.remove(&ids[1]);
    rundown.move_step(&ids[3], false);
    rundown.move_step(&ids[0], true);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["D", "A", "C"], Some("D")));
    rundown.taken(&ids[3], true, T0, 0.0);
    assert_eq!((last(&rundown), next(&rundown)), (Some("D"), Some("A")), "by the list again");
}

/// A step moved or copied right after the one the show is on is what GO
/// takes: the list is the order.
#[test]
fn go_follows_the_list_as_it_is_changed() {
    let (mut rundown, ids) = abcd();
    assert!(rundown.move_step(&ids[0], true));
    assert_eq!(names(&rundown), ["B", "A", "C", "D"]);
    assert!(rundown.move_step(&ids[0], false));
    assert_eq!(names(&rundown), ["A", "B", "C", "D"]);
    assert!(!rundown.move_step(&ids[0], false), "already first");
    assert!(!rundown.move_step(&ids[3], true), "already last");
    assert!(!rundown.move_step("nosuchid", true));
    assert_eq!(names(&rundown), ["A", "B", "C", "D"]);

    rundown.taken(&ids[1], true, T0, 0.0);
    rundown.move_step(&ids[3], false);
    assert_eq!((names(&rundown), next(&rundown)), (vec!["A", "B", "D", "C"], Some("D")));
}

/// A copy keeps everything a step sets, under a new UUID, right after it.
#[test]
fn a_copy_goes_right_after_its_step_with_all_it_sets() {
    let (mut rundown, ids) = abcd();
    let b = &ids[1];
    rundown.set_screen(b, "left", Some(ch("slides")));
    rundown.set_main(b, Some(MainSource::Channel("camera".into())));
    rundown.set_overlay(b, "title", Some(true));
    rundown.set_play(b, "sting", Some(PlayAction::Start));
    rundown.set_then(b, Some(4.0));

    let copy = rundown.duplicate(b).expect("a copy");
    assert_eq!(names(&rundown), ["A", "B", "B copy", "C", "D"]);
    assert_ne!(&copy, b);
    let (original, copied) = (rundown.step(b).unwrap(), rundown.step(&copy).unwrap());
    assert_eq!(
        (&copied.screens, &copied.main, &copied.overlays, &copied.plays, copied.then_secs),
        (&original.screens, &original.main, &original.overlays, &original.plays, Some(4.0))
    );
    // Changing the copy leaves the original alone.
    rundown.set_main(&copy, None);
    assert!(rundown.step(b).unwrap().main.is_some());
    assert!(rundown.duplicate("nosuchid").is_none());
}

/// A step with a wait leads to the step after it by itself — when the wait
/// is up and not before, cut or faded as the step was taken, and once.
#[test]
fn a_wait_leads_to_the_next_step_when_it_is_up() {
    let (mut rundown, ids) = abcd();
    rundown.set_then(&ids[0], Some(5.0));
    rundown.taken(&ids[0], false, T0, 0.0);
    let follow = rundown.follow().expect("counting").clone();
    assert_eq!(
        follow,
        Follow::at(ids[0].clone(), ids[1].clone(), T0 + 5.0, false)
    );
    assert_eq!(rundown.due(T0 + 4.99), None);
    assert_eq!(rundown.due(T0 + 5.0), Some(&follow));
    // Up, it stays up until the step is taken or the wait is held: a step
    // that cannot be taken on this frame may be on the next.
    assert_eq!(rundown.due(T0 + 5.5), Some(&follow));
    rundown.taken(&ids[1], false, T0 + 5.5, 0.0);
    assert!(rundown.follow().is_none(), "B has no wait of its own");
    assert_eq!(rundown.due(T0 + 99.0), None);
    rundown.taken(&ids[0], false, T0, 0.0);
    assert!(rundown.hold(0.0));
    assert_eq!(rundown.due(T0 + 99.0), None);
    // Faded to over longer than its wait, it goes on when its own fade is
    // done; over less, when the wait is up.
    rundown.taken(&ids[0], true, T0, 8.0);
    assert_eq!(rundown.follow().map(|f| f.due), Some(T0 + 8.0));
    rundown.taken(&ids[0], true, T0, 2.0);
    assert_eq!(rundown.follow().map(|f| f.due), Some(T0 + 5.0));
    // The last step has nothing to lead to: no wait starts.
    rundown.set_then(&ids[3], Some(1.0));
    rundown.taken(&ids[3], true, T0, 0.0);
    assert!(rundown.follow().is_none());
}

/// A wait is over the moment the operator does something else: another step
/// taken, the next step chosen, Hold, or the list changed.
#[test]
fn a_wait_is_dropped_when_the_operator_steps_in() {
    type Change = fn(&mut Rundown, &[String; 4]);
    let (mut rundown, ids) = abcd();
    let arm = |rundown: &mut Rundown| {
        rundown.set_then(&ids[0], Some(5.0));
        rundown.taken(&ids[0], true, T0, 0.0);
        assert!(rundown.follow().is_some());
    };
    let changes: [(&str, Change); 10] = [
        ("another step taken", |r, ids| {
            r.taken(&ids[2], true, T0 + 1.0, 0.0);
        }),
        ("the next step chosen", |r, ids| {
            r.set_next(&ids[3]);
        }),
        ("hold", |r, _| {
            assert!(r.hold(0.0));
            assert!(!r.hold(0.0), "nothing left to hold");
        }),
        ("a step renamed", |r, ids| {
            r.rename(&ids[1], "B2");
        }),
        ("a step edited", |r, ids| {
            r.set_main(&ids[1], Some(MainSource::Black));
        }),
        ("a step moved", |r, ids| {
            r.move_step(&ids[2], true);
        }),
        ("a step copied", |r, ids| {
            r.duplicate(&ids[0]);
        }),
        ("a step added", |r, _| {
            r.add(Step::new("E"));
        }),
        ("a step removed", |r, ids| {
            r.remove(&ids[3]);
        }),
        ("undo", |r, _| {
            let as_it_is = r.to_config();
            r.restore_steps(&as_it_is);
        }),
    ];
    for (what, change) in changes {
        let mut rundown = rundown.clone();
        arm(&mut rundown);
        change(&mut rundown, &ids);
        assert!(rundown.follow().is_none(), "{what}");
        assert_eq!(rundown.due(T0 + 60.0), None, "{what}");
    }
    // Held, the show is still on A.
    arm(&mut rundown);
    rundown.hold(0.0);
    assert_eq!((last(&rundown), next(&rundown)), (Some("A"), Some("B")));
}

/// Where a wait leads is fixed when it starts. Something deleted under it
/// drops it; a step it leads to that is gone is not replaced by another.
#[test]
fn a_wait_leads_where_it_said_or_nowhere() {
    let (mut rundown, ids) = abcd();
    rundown.set_then(&ids[0], Some(5.0));
    rundown.set_screen(&ids[2], "left", Some(ch("slides")));
    rundown.taken(&ids[0], true, T0, 0.0);
    assert!(rundown.forget_bus("right").then_some(()).is_none(), "no step sets it");
    assert!(rundown.follow().is_some(), "nothing changed, so the wait goes on");
    assert_eq!(rundown.forget_source("slides"), ["C"]);
    assert!(rundown.follow().is_none(), "a step changed under it");

    rundown.set_screen(&ids[2], "left", Some(ch("logo")));
    rundown.taken(&ids[0], true, T0, 0.0);
    assert!(rundown.forget_bus("left"));
    assert!(rundown.follow().is_none(), "a step changed under it");
}

#[test]
fn a_wait_must_be_a_time_more_than_none() {
    let (mut rundown, ids) = abcd();
    for secs in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
        rundown.set_then(&ids[0], Some(secs));
        assert_eq!(rundown.step(&ids[0]).unwrap().then_secs, None, "{secs}");
    }
    rundown.set_then(&ids[0], Some(0.5));
    assert_eq!(rundown.step(&ids[0]).unwrap().then_secs, Some(0.5));
    rundown.set_then(&ids[0], Some(1e9));
    assert_eq!(rundown.step(&ids[0]).unwrap().then_secs, Some(MAX_THEN_SECS));
    rundown.set_then(&ids[0], None);
    assert_eq!(rundown.step(&ids[0]).unwrap().then_secs, None);
    assert!(!rundown.set_then("nosuchid", Some(1.0)));
}

/// Each bus, overlay and deck is set once in a step; set again it changes,
/// set to nothing it is left alone.
#[test]
fn a_step_sets_each_thing_once() {
    let (mut rundown, ids) = abcd();
    let a = &ids[0];
    assert!(rundown.set_screen(a, "left", Some(ch("intro"))));
    assert!(rundown.set_screen(a, "right", Some(BusInputRef::Master)));
    assert!(rundown.set_screen(a, "left", Some(ch("slides"))));
    assert_eq!(
        rundown.step(a).unwrap().screen_sets(),
        [("left".to_string(), ch("slides")), ("right".to_string(), BusInputRef::Master)]
    );
    assert!(rundown.set_screen(a, "left", None));
    assert!(rundown.set_screen(a, "middle", None));
    assert_eq!(rundown.step(a).unwrap().screens.len(), 1);

    rundown.set_overlay(a, "title", Some(true));
    rundown.set_overlay(a, "title", Some(false));
    assert_eq!(
        rundown.step(a).unwrap().overlays,
        [OverlaySet { channel: "title".into(), on: false }]
    );
    rundown.set_overlay(a, "title", None);
    assert!(rundown.step(a).unwrap().overlays.is_empty());

    rundown.set_play(a, "sting", Some(PlayAction::Start));
    rundown.set_play(a, "sting", Some(PlayAction::Pause));
    assert_eq!(
        rundown.step(a).unwrap().plays,
        [PlaySet::new("sting", PlayAction::Pause)]
    );
    assert!(!rundown.step(a).unwrap().is_empty());
    rundown.set_play(a, "sting", None);

    rundown.set_main(a, Some(MainSource::Channel("camera".into())));
    assert!(rundown.step(a).unwrap().sets_picture());
    rundown.set_main(a, None);
    rundown.set_screen(a, "right", None);
    assert!(rundown.step(a).unwrap().is_empty() && !rundown.step(a).unwrap().sets_picture());
    assert!(!rundown.set_screen("nosuchid", "left", Some(ch("intro"))));
    assert!(rundown.rename(a, "A Opening"));
    assert_eq!(rundown.step(a).unwrap().name, "A Opening");
}

/// A deleted channel or deck leaves every step that named it; a removed bus
/// leaves every step's screens. The rest of each step stays, and the steps
/// that changed are named.
#[test]
fn steps_let_go_of_what_is_deleted() {
    let (mut rundown, ids) = abcd();
    let a = &ids[0];
    rundown.set_screen(a, "left", Some(ch("slides")));
    rundown.set_screen(a, "right", Some(BusInputRef::Deck("sting".into())));
    rundown.set_screen(a, "middle", Some(ch("logo")));
    rundown.set_main(a, Some(MainSource::Channel("slides".into())));
    rundown.set_overlay(a, "slides", Some(true));
    rundown.set_overlay(a, "title", Some(true));
    rundown.set_play(a, "sting", Some(PlayAction::Start));
    rundown.set_play(a, "music", Some(PlayAction::Stop));
    rundown.set_main(&ids[1], Some(MainSource::Deck("sting".into())));

    assert_eq!(rundown.forget_source("slides"), ["A"]);
    let step = rundown.step(a).unwrap();
    assert_eq!(step.screens.len(), 2);
    assert_eq!(step.main, None);
    assert_eq!(step.overlays.len(), 1);
    assert!(rundown.forget_source("slides").is_empty(), "nothing left of it");

    assert_eq!(rundown.forget_source("sting"), ["A", "B"]);
    let step = rundown.step(a).unwrap();
    assert_eq!(step.screen_sets(), [("middle".to_string(), ch("logo"))]);
    assert_eq!(step.plays, [PlaySet::new("music", PlayAction::Stop)]);
    assert_eq!(rundown.step(&ids[1]).unwrap().main, None);

    assert!(rundown.forget_bus("middle"));
    assert!(rundown.step(a).unwrap().screens.is_empty());
    assert!(!rundown.forget_bus("middle"));
}

/// Undo puts the steps back and leaves the show where it is: on the same
/// step if it is still there, else on the nearest before it.
#[test]
fn undo_puts_steps_back_without_moving_the_show() {
    let (mut rundown, ids) = abcd();
    rundown.set_fade_secs(3.0);
    let before = rundown.steps().to_vec();
    rundown.taken(&ids[2], true, T0, 0.0);
    rundown.rename(&ids[0], "Opening");
    rundown.remove(&ids[3]);

    rundown.restore_steps(&saved(&before));
    assert_eq!(names(&rundown), ["A", "B", "C", "D"]);
    assert_eq!((last(&rundown), next(&rundown)), (Some("C"), Some("D")));
    assert_eq!(rundown.fade_secs(), 3.0);

    // Undoing the adding of the step the show is on, and of the one before:
    // no step is the one taken last, and the show goes on from the nearest
    // earlier step still there — at the end of what is left.
    rundown.set_next(&ids[3]);
    rundown.restore_steps(&saved(&before[..1]));
    assert_eq!((last(&rundown), next(&rundown)), (None, None));
    // Redone, the show is on C again.
    rundown.restore_steps(&saved(&before));
    assert_eq!((last(&rundown), next(&rundown)), (Some("C"), Some("D")));
}

/// Saved and opened, the rundown is as it was — but for a wait, which is
/// never carried over a restart.
#[test]
fn a_rundown_is_as_it_was_after_a_restart() {
    let (mut rundown, ids) = abcd();
    rundown.set_screen(&ids[0], "left", Some(ch("slides")));
    rundown.set_main(&ids[1], Some(MainSource::Black));
    rundown.set_overlay(&ids[1], "title", Some(true));
    rundown.set_play(&ids[1], "sting", Some(PlayAction::Resume));
    rundown.set_then(&ids[1], Some(4.5));
    rundown.set_fade_secs(2.0);
    rundown.taken(&ids[1], true, T0, 0.0);
    assert!(rundown.follow().is_some());

    let text = serde_json::to_string(&rundown.to_config()).expect("written");
    let opened = Rundown::from_config(serde_json::from_str(&text).expect("read"));
    assert_eq!(opened.steps(), rundown.steps());
    assert_eq!(opened.fade_secs(), 2.0);
    assert_eq!((last(&opened), next(&opened)), (Some("B"), Some("C")));
    assert!(opened.follow().is_none());
    // A step that sets nothing is written as its name alone; an action by
    // its plain name.
    assert!(!text.contains("\"screens\":[]") && !text.contains("then_secs\":null"), "{text}");
    assert!(text.contains("\"action\":\"resume\""), "{text}");

    // And the step the operator said comes next.
    rundown.set_next(&ids[3]);
    let text = serde_json::to_string(&rundown.to_config()).expect("written");
    let opened = Rundown::from_config(serde_json::from_str(&text).expect("read"));
    assert_eq!((last(&opened), next(&opened)), (Some("B"), Some("D")));
}

/// A file written by hand cannot keep the show from opening, or make a step
/// nothing can name. What a step sets on a bus or an input that is not there
/// is kept, for the step to say so.
#[test]
fn a_bad_file_is_put_right_not_refused() {
    let config: RundownConfig = serde_json::from_str(
        r#"{
            "steps": [
                {"uuid": "aaaa0001", "name": "A",
                 "screens": [{"bus": "left", "input": "Black"}, {"bus": "left", "input": "Master"},
                             {"bus": "gone", "input": {"Channel": "gone too"}}],
                 "then_secs": -3.0},
                {"uuid": "aaaa0001", "name": "twin"},
                {"uuid": "  ", "name": "nameless"},
                {"uuid": "aaaa0002", "name": "B"}
            ],
            "fade_secs": -1.0,
            "last": "gone",
            "next": "gone"
        }"#,
    )
    .expect("read");
    let rundown = Rundown::from_config(config);
    assert_eq!(names(&rundown), ["A", "B"]);
    assert_eq!(
        rundown.steps()[0].screen_sets(),
        [("left".to_string(), BusInputRef::Black), ("gone".to_string(), ch("gone too"))]
    );
    assert_eq!(rundown.steps()[0].then_secs, None);
    assert_eq!(rundown.fade_secs(), DEFAULT_FADE_SECS);
    // A step taken last that is not in the list is a step since removed: no
    // step is marked, and GO takes nothing rather than the top again.
    assert_eq!((rundown.last(), next(&rundown)), (None, None));
    // Nothing at all: an empty rundown.
    let empty = Rundown::from_config(serde_json::from_str("{}").expect("read"));
    assert!(empty.steps().is_empty() && empty.next().is_none());
}

#[test]
fn a_fade_time_must_be_a_time() {
    let mut rundown = Rundown::default();
    for secs in [0.0, -2.0, f32::NAN, f32::INFINITY] {
        assert!(!rundown.set_fade_secs(secs), "{secs}");
        assert_eq!(rundown.fade_secs(), DEFAULT_FADE_SECS);
    }
    assert!(rundown.set_fade_secs(0.5));
    assert_eq!(rundown.fade_secs(), 0.5);
}

/// New steps are lettered as a running order is, skipping letters in use.
#[test]
fn a_new_step_is_named_by_the_first_letter_free() {
    assert_eq!(new_name(&[]), "A");
    assert_eq!(new_name(&["A", "B Walk-on", "Break"]), "C");
    assert_eq!(new_name(&["B Walk-on", "Break", "C"]), "A");
    let letters: Vec<String> = ('A'..='Z').map(String::from).collect();
    let all: Vec<&str> = letters.iter().map(String::as_str).collect();
    assert_eq!(new_name(&all), "Step 27");
}

/// A step's time of day: due as the clock comes to it — while it is the
/// step GO takes next, not held, and no more than a minute late — across
/// midnight too. Hold holds it till a step is taken, it is made next again
/// or its time is changed.
#[test]
fn a_step_is_due_at_its_time_of_day_while_it_is_next() {
    let (mut rundown, [a, b, _, _]) = abcd();
    let nine = 9 * 3600;
    assert!(rundown.set_at(&a, Some(nine), None));
    assert!(rundown.set_at(&b, Some(nine + 900), None));
    let at = |secs: u32| f64::from(secs);
    let due = |rundown: &Rundown, was: f64, now: f64| rundown.due_at(was, now).map(|s| s.uuid.clone());

    // A is next: due as the clock comes to 9:00 — not before, once, and not
    // by a Mac waking five minutes after it.
    assert_eq!(due(&rundown, at(nine) - 1.0, at(nine) - 0.5), None, "not yet");
    assert_eq!(due(&rundown, at(nine) - 0.5, at(nine) + 0.01), Some(a.clone()));
    assert_eq!(due(&rundown, at(nine) + 0.01, at(nine) + 0.02), None, "once");
    assert_eq!(due(&rundown, at(nine) - 60.0, at(nine) + 300.0), None, "long after");
    // B is not due while A is next.
    assert_eq!(due(&rundown, at(nine + 899), at(nine + 901)), None);

    // A taken: B is next, due at 9:15.
    assert!(rundown.taken(&a, true, T0, 0.0));
    assert_eq!(due(&rundown, at(nine + 899), at(nine + 900)), Some(b.clone()));
    // Held: not due, till it is made next again.
    assert!(rundown.hold(at(nine)));
    assert_eq!(rundown.at_held(), Some(b.as_str()));
    assert_eq!(due(&rundown, at(nine + 899), at(nine + 900)), None, "held");
    assert!(rundown.set_next(&b));
    assert_eq!(rundown.at_held(), None);
    assert_eq!(due(&rundown, at(nine + 899), at(nine + 900)), Some(b.clone()));
    // Its time changed lets a hold go too; a step taken does.
    assert!(rundown.hold(at(nine)));
    assert!(rundown.set_at(&b, Some(nine + 1800), None));
    assert_eq!(rundown.at_held(), None);
    assert!(rundown.hold(at(nine)));
    assert!(rundown.taken(&a, false, T0, 0.0));
    assert_eq!(rundown.at_held(), None);

    // Across midnight: at 00:00:30, due from 23:59:59 to 00:00:31.
    assert!(rundown.set_at(&b, Some(30), None));
    assert_eq!(due(&rundown, 86_399.0, 31.0), Some(b.clone()));
    // Due, it is taken as a wait run out is: now, faded.
    assert!(rundown.start_at(&b, T0));
    let follow = rundown.due(T0).expect("due now");
    assert_eq!(follow.to, b);
    assert!(follow.fade);
    // A time past the day is none.
    assert!(rundown.set_at(&a, Some(DAY_SECS), None));
    assert_eq!(rundown.step(&a).unwrap().at_secs, None);
}

/// What a step plays till the next step, and how it sets a timer it
/// starts: kept on the step's entry for it — only one it has — through a
/// change of action, saved, and read as none from a file without them.
#[test]
fn a_steps_play_keeps_its_fade_out_and_its_timer() {
    let (mut rundown, [a, _, _, _]) = abcd();
    assert!(!rundown.set_until_next(&a, "music", true), "not one the step plays");
    assert!(!rundown.set_timer(&a, "clock", Some(TimerSet::Up)));
    rundown.set_play(&a, "music", Some(PlayAction::Start));
    rundown.set_play(&a, "clock", Some(PlayAction::Start));
    assert!(rundown.set_until_next(&a, "music", true));
    assert!(rundown.set_timer(&a, "clock", Some(TimerSet::Down { secs: 0 })));
    let plays = &rundown.step(&a).unwrap().plays;
    assert!(plays[0].until_next);
    assert_eq!(plays[1].timer, Some(TimerSet::Down { secs: 1 }), "a second at least");

    rundown.set_play(&a, "music", Some(PlayAction::Resume));
    assert!(rundown.step(&a).unwrap().plays[0].until_next, "kept");

    let json = serde_json::to_string(&rundown.to_config()).unwrap();
    let back = Rundown::from_config(serde_json::from_str(&json).unwrap());
    assert_eq!(back.step(&a).unwrap().plays, rundown.step(&a).unwrap().plays);
    let old: PlaySet = serde_json::from_str(r#"{"deck":"d","action":"start"}"#).unwrap();
    assert_eq!(old, PlaySet::new("d", PlayAction::Start));
    let up: PlaySet =
        serde_json::from_str(r#"{"deck":"d","action":"start","timer":"up"}"#).unwrap();
    assert_eq!(up.timer, Some(TimerSet::Up));
    let down: PlaySet =
        serde_json::from_str(r#"{"deck":"d","action":"start","timer":{"down":{"secs":900}}}"#)
            .unwrap();
    assert_eq!(down.timer, Some(TimerSet::Down { secs: 900 }));
}

/// The clock set back, or summer time ending, takes no step whose time it
/// reads again; midnight does, even for a time just before it.
#[test]
fn only_midnight_is_a_step_back_of_the_clock_that_takes_a_step() {
    let (mut rundown, [a, _, _, _]) = abcd();
    assert!(rundown.set_at(&a, Some(9 * 3600), None));
    // From 10:00:05 back to 09:00:10: no forward crossing of 9:00.
    assert!(rundown.due_at(36_005.0, 32_410.0).is_none(), "set back");
    // From 09:00:30 back to 09:00:10 — a nudge back — neither.
    assert!(rundown.due_at(32_430.0, 32_410.0).is_none(), "nudged back");
    // A step at 23:59:59: due from 23:59:58.8 to 00:00:00.1.
    assert!(rundown.set_at(&a, Some(DAY_SECS - 1), None));
    assert!(rundown.due_at(86_398.8, 0.1).is_some(), "just before midnight");
    assert!(rundown.due_at(86_399.5, 0.1).is_none(), "already past it");
}

/// A step taken at its time even when not next: due as the clock comes to
/// it wherever the rundown is — not when the show is on it already, not
/// while held, and its hold stays through steps taken meanwhile till its
/// time is set again. The countdown and Hold go by the timed step coming
/// first. Without a time it is not one.
#[test]
fn a_step_at_its_time_even_when_not_next() {
    let (mut rundown, [a, b, c, _]) = abcd();
    let at = |secs: u32| f64::from(secs);
    let quarter = 9 * 3600 + 900;
    assert!(rundown.set_at(&c, Some(quarter), Some(true)));
    assert!(rundown.step(&c).unwrap().at_always);
    assert!(rundown.taken(&a, false, T0, 0.0));
    let due = |r: &Rundown, was: f64, now: f64| r.due_always(was, now).map(|s| s.uuid.clone());
    // B is next; C is due at 9:15 all the same, and only C.
    assert_eq!(rundown.due_at(at(quarter) - 0.5, at(quarter) + 0.1), None, "B has no time");
    assert_eq!(due(&rundown, at(quarter) - 0.5, at(quarter) + 0.1), Some(c.clone()));
    assert_eq!(due(&rundown, at(quarter) + 0.1, at(quarter) + 0.2), None, "once");
    // The countdown: C, B having no time.
    assert_eq!(rundown.coming_at(at(9 * 3600)).map(|s| s.uuid.clone()), Some(c.clone()));
    // On it already: not again.
    assert!(rundown.taken(&c, true, T0, 0.0));
    assert_eq!(due(&rundown, at(quarter) - 0.5, at(quarter) + 0.1), None, "on it");
    // Held before its time: not due, held through steps taken meanwhile.
    assert!(rundown.taken(&a, false, T0, 0.0));
    assert!(rundown.hold(at(9 * 3600)));
    assert!(rundown.is_held(&c));
    assert!(rundown.taken(&b, false, T0, 0.0));
    assert!(rundown.set_next(&a));
    assert!(rundown.is_held(&c), "kept");
    assert_eq!(due(&rundown, at(quarter) - 0.5, at(quarter) + 0.1), None, "held");
    // Its time set again lets the hold go.
    assert!(rundown.set_at(&c, Some(quarter), None));
    assert!(!rundown.is_held(&c));
    assert!(rundown.step(&c).unwrap().at_always, "None leaves it");
    assert_eq!(due(&rundown, at(quarter) - 0.5, at(quarter) + 0.1), Some(c.clone()));
    // The next step's time first, when it comes first.
    assert!(rundown.set_at(&a, Some(9 * 3600 + 600), None));
    assert_eq!(rundown.coming_at(at(9 * 3600)).map(|s| s.uuid.clone()), Some(a.clone()));
    assert_eq!(rundown.coming_at(at(9 * 3600 + 700)).map(|s| s.uuid.clone()), Some(c.clone()));
    // Not "always" without a time; not "always" any more, not due.
    assert!(rundown.set_at(&c, None, None));
    assert!(!rundown.step(&c).unwrap().at_always);
    assert!(rundown.set_at(&c, Some(quarter), Some(false)));
    assert_eq!(due(&rundown, at(quarter) - 0.5, at(quarter) + 0.1), None);
}

/// Saved, a step taken at its time even when not next says so; one that
/// is not, says nothing of it.
#[test]
fn even_when_not_next_is_saved_only_when_so() {
    let mut step = Step::new("Keynote");
    step.at_secs = Some(9 * 3600);
    assert!(!serde_json::to_string(&step).unwrap().contains("at_always"));
    step.at_always = true;
    let json = serde_json::to_string(&step).unwrap();
    assert!(json.contains(r#""at_always":true"#), "{json}");
    let back: Step = serde_json::from_str(&json).unwrap();
    assert!(back.at_always);
    let mut timeless: Step = serde_json::from_str(&json.replace(r#""at_secs":32400,"#, "")).unwrap();
    timeless.tidy();
    assert!(!timeless.at_always, "not without a time");
}

/// Holds side by side: one taken even when not next stays held when the
/// next step's time is held too. At one time, the step next goes first —
/// the countdown says the one that is taken.
#[test]
fn holds_side_by_side_and_the_next_first_at_one_time() {
    let (mut rundown, [a, b, c, _]) = abcd();
    let at = |secs: u32| f64::from(secs);
    let (ten, quarter) = (9 * 3600 + 600, 9 * 3600 + 900);
    assert!(rundown.set_at(&c, Some(quarter), Some(true)));
    assert!(rundown.taken(&a, false, T0, 0.0));
    assert!(rundown.hold(at(9 * 3600)), "C, B having no time");
    assert!(rundown.is_held(&c));
    assert!(rundown.set_at(&b, Some(ten), None));
    assert!(rundown.hold(at(9 * 3600)), "B now, its time first");
    assert_eq!(rundown.at_held(), Some(b.as_str()));
    assert!(rundown.is_held(&c), "C still held");
    assert_eq!(rundown.due_always(at(quarter) - 0.5, at(quarter) + 0.1), None);
    assert!(!rundown.hold(at(9 * 3600)), "nothing more to hold");

    // At one time: B, next, goes first; the countdown says B.
    let (mut rundown, [a, b, c, _]) = abcd();
    assert!(rundown.set_at(&a, Some(quarter), Some(true)));
    assert!(rundown.set_at(&c, Some(quarter), None));
    assert!(rundown.taken(&b, false, T0, 0.0));
    assert_eq!(rundown.next().map(|s| s.uuid.clone()), Some(c.clone()));
    assert_eq!(rundown.coming_at(at(9 * 3600)).map(|s| s.uuid.clone()), Some(c.clone()));
    assert_eq!(
        rundown.due_at(at(quarter) - 0.5, at(quarter) + 0.1).map(|s| s.uuid.clone()),
        Some(c),
        "the one counted down to"
    );
    let _ = a;
}

/// Undo turning "even when not next" back under a hold keeps the hold
/// where that step's holds are now: made next again lets it go.
#[test]
fn undo_moves_a_hold_to_where_the_steps_holds_are() {
    let (mut rundown, [a, b, _, _]) = abcd();
    let quarter = 9 * 3600 + 900;
    assert!(rundown.set_at(&b, Some(quarter), None));
    assert!(rundown.taken(&a, false, T0, 0.0));
    let before = rundown.to_config();
    assert!(rundown.set_at(&b, Some(quarter), Some(true)));
    assert!(rundown.hold(f64::from(9 * 3600)));
    assert_eq!(rundown.held_always(), std::slice::from_ref(&b));
    rundown.restore_steps(&before);
    assert!(!rundown.step(&b).unwrap().at_always);
    assert_eq!(rundown.at_held(), Some(b.as_str()), "the next step's hold now");
    assert!(rundown.held_always().is_empty());
    assert!(rundown.set_next(&b));
    assert!(!rundown.is_held(&b), "let go as the next step's is");

    // A hold left on another step does not keep the next step's from
    // coming over: B held, then moved after C; C ticked, held, untoggled.
    let (mut rundown, [a, b, c, _]) = abcd();
    assert!(rundown.set_at(&b, Some(quarter - 300), None));
    assert!(rundown.set_at(&c, Some(quarter), None));
    assert!(rundown.taken(&a, false, T0, 0.0));
    assert!(rundown.hold(f64::from(9 * 3600)));
    assert_eq!(rundown.at_held(), Some(b.as_str()));
    let mut moved = rundown.to_config();
    let at = moved.steps.iter().position(|s| s.uuid == b).unwrap();
    let step = moved.steps.remove(at);
    moved.steps.insert(at + 1, step);
    rundown.restore_steps(&moved);
    assert_eq!(rundown.next().map(|s| s.uuid.clone()), Some(c.clone()));
    let before = rundown.to_config();
    assert!(rundown.set_at(&c, Some(quarter), Some(true)));
    assert!(rundown.hold(f64::from(9 * 3600)));
    assert!(rundown.held_always().contains(&c));
    rundown.restore_steps(&before);
    assert_eq!(rundown.at_held(), Some(c.as_str()), "C, next, held still");
    assert_eq!(rundown.due_at(f64::from(quarter) - 0.5, f64::from(quarter) + 0.1), None);
}

/// A step goes on to the next when a clip or playlist it starts ends: set
/// only for one it starts or carries on, instead of a time; gone when that
/// one is taken out or paused. Taken, it waits — no time known — till the
/// app has seen it playing, then at its end; no sooner than the take's own
/// fades are done.
#[test]
fn a_step_goes_on_when_what_it_plays_ends() {
    let (mut rundown, [a, b, _, _]) = abcd();
    assert!(!rundown.set_then_ends(&a, Some("music")), "A does not play it");
    assert!(rundown.set_play(&a, "music", Some(PlayAction::Start)));
    assert!(rundown.set_play(&a, "video", Some(PlayAction::Start)));
    assert!(rundown.set_then(&a, Some(5.0)));
    assert!(rundown.set_then_ends(&a, Some("music")));
    let step = rundown.step(&a).unwrap();
    assert_eq!((step.then_ends.as_deref(), step.then_secs), (Some("music"), None), "not both");

    assert!(rundown.taken(&a, true, T0, 2.0));
    assert_eq!(rundown.awaited_end(), Some("music"));
    assert_eq!(rundown.due(T0 + 999.0), None, "no time");
    assert!(!rundown.end_came(T0 + 0.1), "an end before it was seen playing");
    rundown.saw_running();
    assert!(rundown.end_came(T0 + 0.5));
    assert_eq!(rundown.due(T0 + 1.9), None, "not before the fades are done");
    assert_eq!(rundown.due(T0 + 2.0).map(|f| f.to.clone()), Some(b.clone()));
    assert_eq!(rundown.awaited_end(), None, "due now");

    // A time set lets the end go; and taking the clip out, or pausing it.
    assert!(rundown.set_then(&a, Some(3.0)));
    assert_eq!(rundown.step(&a).unwrap().then_ends, None);
    assert!(rundown.set_then_ends(&a, Some("music")));
    assert!(rundown.set_play(&a, "music", Some(PlayAction::Pause)));
    assert_eq!(rundown.step(&a).unwrap().then_ends, None, "paused: no end");
    assert!(rundown.set_play(&a, "music", Some(PlayAction::Start)));
    assert!(rundown.set_then_ends(&a, Some("music")));
    assert!(rundown.set_play(&a, "music", None));
    assert_eq!(rundown.step(&a).unwrap().then_ends, None, "taken out");

    // Saved and read back.
    let mut step = Step::new("Walk-in");
    step.plays.push(PlaySet::new("music", PlayAction::Start));
    step.then_ends = Some("music".into());
    let json = serde_json::to_string(&step).unwrap();
    assert!(json.contains(r#""then_ends":"music""#), "{json}");
    let back: Step = serde_json::from_str(&json).unwrap();
    assert_eq!(back.then_ends.as_deref(), Some("music"));
}

