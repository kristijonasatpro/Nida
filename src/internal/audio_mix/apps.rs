//! Apps' sound — a Zoom or Teams call's — taken on a thread of its own.
//!
//! Listing the processes that make sound, tapping one and opening its tap's
//! device all wait on the sound server; on the render thread they would
//! freeze the picture, as finding a missing bus device once did (see
//! `outputs`). The render thread only says which apps its inputs want and
//! mixes the sound handed back.
//!
//! What is wanted is (input UUID, app, generation). The generation moves on
//! when the render thread drops an input's sound itself — a show loaded over
//! it — so the input is tapped afresh even if the same show comes straight
//! back; every event carries all three, so a late one for an old app or
//! generation is told apart.

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::audio::app_sound::{apps_of, RunningApp};

use super::{LiveFeed, RETRY};

/// An app's sound being taken: dropping it lets the app go.
pub(super) trait Taken {
    /// Whether its device has reported an error.
    fn failed(&self) -> bool;
}

/// What the thread asks the system: the real thing, or a stand-in for tests.
pub(super) trait Taps: Send + 'static {
    /// Every process the sound server knows: (its audio object, the bundle
    /// ID of the app it belongs to, making sound now).
    fn processes(&mut self) -> Vec<(u32, String, bool)>;
    /// What an app calls itself.
    fn name(&mut self, bundle_id: &str) -> String;
    /// Tap the processes `processes` of the app `bundle_id`, and start
    /// reading its sound.
    fn take(
        &mut self,
        bundle_id: &str,
        processes: &[u32],
    ) -> anyhow::Result<(Box<dyn Taken>, LiveFeed)>;
}

/// An app an input wants: (input UUID, app bundle ID, generation).
pub(super) type Want = (String, String, u64);

/// What the thread hands back.
pub(super) enum Event {
    /// An input's app is tapped: its sound, to mix.
    Taken {
        uuid: String,
        app: String,
        generation: u64,
        feed: LiveFeed,
    },
    /// An input's app runs, but its sound could not be taken, and why.
    Refused {
        uuid: String,
        app: String,
        generation: u64,
        why: String,
    },
    /// An input's app was let go — quit, restarted, its input removed — or,
    /// refused before, is no longer running: its sound stops, its refusal is
    /// forgotten.
    Lost {
        uuid: String,
        app: String,
        generation: u64,
    },
    /// The apps whose sound can be taken, when the list changes.
    Listed(Vec<RunningApp>),
}

/// The thread taking apps' sound. Dropping it stops the thread and lets every
/// app go.
pub(super) struct Apps {
    wanted: Option<mpsc::Sender<Vec<Want>>>,
    events: mpsc::Receiver<Event>,
    told: Vec<Want>,
    thread: Option<JoinHandle<()>>,
}

impl Apps {
    /// Start the thread, asking the system through `taps`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to start the thread.
    pub(super) fn spawn(taps: Box<dyn Taps>) -> Self {
        let (wanted_tx, wanted_rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("app-sound".into())
            .spawn(move || run(taps, &wanted_rx, &events_tx))
            .expect("failed to start the app sound thread");
        Self {
            wanted: Some(wanted_tx),
            events,
            told: Vec::new(),
            thread: Some(thread),
        }
    }

    /// The apps the inputs want: told to the thread when it changes.
    pub(super) fn want(&mut self, wanted: Vec<Want>) {
        if wanted == self.told {
            return;
        }
        if let Some(tx) = &self.wanted {
            let _ = tx.send(wanted.clone());
        }
        self.told = wanted;
    }

    /// What the thread has handed back since last asked.
    pub(super) fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Apps {
    fn drop(&mut self) {
        // Hanging up wakes the thread, which lets every app go and ends.
        self.wanted = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An input's app as the thread holds it: which app, which generation, the
/// processes tapped, and the sound being taken.
struct Held {
    app: String,
    generation: u64,
    processes: Vec<u32>,
    taken: Box<dyn Taken>,
}

/// Look every [`RETRY`] — or at once, when the wanted apps change — at the
/// processes making sound: list the apps, tap each wanted app while it runs,
/// tap it afresh when its processes change (it was restarted) or its device
/// fails, and let it go when it is no longer wanted.
fn run(
    mut taps: Box<dyn Taps>,
    wanted_rx: &mpsc::Receiver<Vec<Want>>,
    events: &mpsc::Sender<Event>,
) {
    let mut wanted: Vec<Want> = Vec::new();
    let mut held: HashMap<String, Held> = HashMap::new();
    let mut listed: Vec<RunningApp> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();
    // Each input's refusal said to the engine: its app, generation and why.
    let mut told: HashMap<String, (String, u64, String)> = HashMap::new();
    // Why each app was refused, as logged: once, however many inputs ask.
    let mut logged: HashMap<String, String> = HashMap::new();
    loop {
        match wanted_rx.recv_timeout(RETRY) {
            Ok(latest) => wanted = wanted_rx.try_iter().last().unwrap_or(latest),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let processes = taps.processes();

        let found: Vec<(String, bool)> = processes
            .iter()
            .map(|(_, bundle, playing)| (bundle.clone(), *playing))
            .collect();
        let apps: Vec<RunningApp> = apps_of(&found)
            .into_iter()
            .map(|(bundle_id, playing)| RunningApp {
                name: names
                    .entry(bundle_id.clone())
                    .or_insert_with(|| taps.name(&bundle_id))
                    .clone(),
                bundle_id,
                playing,
            })
            .collect();
        if apps != listed {
            listed.clone_from(&apps);
            let _ = events.send(Event::Listed(apps));
        }

        let of = |app: &str| -> Vec<u32> {
            let mut ids: Vec<u32> = processes
                .iter()
                .filter(|(_, owner, _)| owner == app)
                .map(|(process, ..)| *process)
                .collect();
            ids.sort_unstable();
            ids
        };
        let gone: Vec<String> = held
            .iter()
            .filter(|(uuid, h)| {
                let still = wanted.iter().any(|(w, app, generation)| {
                    w == *uuid
                        && *app == h.app
                        && *generation == h.generation
                        && of(app) == h.processes
                });
                !still || h.taken.failed()
            })
            .map(|(uuid, _)| uuid.clone())
            .collect();
        for uuid in gone {
            if let Some(h) = held.remove(&uuid) {
                let _ = events.send(Event::Lost {
                    uuid,
                    app: h.app,
                    generation: h.generation,
                });
            }
        }
        told.retain(|uuid, (app, generation, _)| {
            wanted
                .iter()
                .any(|(w, a, g)| w == uuid && a == app && g == generation)
        });
        for (uuid, app, generation) in &wanted {
            let tapping = of(app);
            if tapping.is_empty() && told.remove(uuid).is_some() {
                // Refused while it ran; not running now.
                let _ = events.send(Event::Lost {
                    uuid: uuid.clone(),
                    app: app.clone(),
                    generation: *generation,
                });
            }
            if held.contains_key(uuid) || tapping.is_empty() {
                continue;
            }
            match taps.take(app, &tapping) {
                Ok((taken, feed)) => {
                    log::info!(
                        "App sound '{app}' taken ({} Hz, {} ch)",
                        feed.rate,
                        feed.channels
                    );
                    told.remove(uuid);
                    logged.remove(app);
                    held.insert(
                        uuid.clone(),
                        Held {
                            app: app.clone(),
                            generation: *generation,
                            processes: tapping,
                            taken,
                        },
                    );
                    let _ = events.send(Event::Taken {
                        uuid: uuid.clone(),
                        app: app.clone(),
                        generation: *generation,
                        feed,
                    });
                }
                Err(e) => {
                    let why = format!("{e:#}");
                    if logged.get(app) != Some(&why) {
                        log::warn!("App sound '{app}' not taken: {why}");
                        logged.insert(app.clone(), why.clone());
                    }
                    let refusal = (app.clone(), *generation, why.clone());
                    if told.get(uuid) != Some(&refusal) {
                        told.insert(uuid.clone(), refusal);
                        let _ = events.send(Event::Refused {
                            uuid: uuid.clone(),
                            app: app.clone(),
                            generation: *generation,
                            why,
                        });
                    }
                }
            }
        }
    }
}

/// The real thing: Core Audio process taps (see [`crate::audio::app_sound`]).
pub(super) struct SystemTaps;

/// A tap and its device being read. Fields drop in order: the device stops
/// before its tap goes.
struct SystemTaken {
    input: super::device::DeviceInput,
    _tap: crate::audio::app_sound::Tap,
}

impl Taken for SystemTaken {
    fn failed(&self) -> bool {
        self.input.failed()
    }
}

impl Taps for SystemTaps {
    fn processes(&mut self) -> Vec<(u32, String, bool)> {
        crate::audio::app_sound::processes()
    }

    fn name(&mut self, bundle_id: &str) -> String {
        crate::audio::app_sound::app_name(bundle_id)
    }

    fn take(
        &mut self,
        bundle_id: &str,
        processes: &[u32],
    ) -> anyhow::Result<(Box<dyn Taken>, LiveFeed)> {
        let tap = crate::audio::app_sound::Tap::create(bundle_id, processes)?;
        let (input, feed) = super::device::open_input(tap.device())?;
        // Checked when the tap was made; the stream must agree.
        if feed.channels != 2 {
            anyhow::bail!("its device opened with {} channels, not the tap's two", feed.channels);
        }
        Ok((Box::new(SystemTaken { input, _tap: tap }), feed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// The processes a stand-in system reports, as the test sets them.
    type Processes = Arc<Mutex<Vec<(u32, String, bool)>>>;
    /// Each app a stand-in system tapped, with the processes.
    type Takes = Arc<Mutex<Vec<(String, Vec<u32>)>>>;

    /// A stand-in system: its processes set by the test, each take counted.
    #[derive(Clone, Default)]
    struct Fake {
        processes: Processes,
        takes: Takes,
        fail: Arc<AtomicBool>,
        refuse: Arc<AtomicBool>,
    }

    struct FakeTaken(Arc<AtomicBool>);

    impl Taken for FakeTaken {
        fn failed(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    impl Taps for Fake {
        fn processes(&mut self) -> Vec<(u32, String, bool)> {
            self.processes.lock().unwrap().clone()
        }

        fn name(&mut self, bundle_id: &str) -> String {
            format!("{bundle_id} app")
        }

        fn take(
            &mut self,
            bundle_id: &str,
            processes: &[u32],
        ) -> anyhow::Result<(Box<dyn Taken>, LiveFeed)> {
            if self.refuse.load(Ordering::Relaxed) {
                anyhow::bail!("refused");
            }
            self.takes
                .lock()
                .unwrap()
                .push((bundle_id.to_string(), processes.to_vec()));
            let (_tx, rx) = crossbeam_channel::bounded(1);
            Ok((
                Box::new(FakeTaken(self.fail.clone())),
                LiveFeed {
                    rx,
                    rate: 48_000,
                    channels: 2,
                },
            ))
        }
    }

    /// The next events, waiting up to a few seconds for one, `kind` picked.
    fn next(apps: &Apps, mut kind: impl FnMut(&Event) -> bool) -> Vec<Event> {
        let until = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < until {
            got.extend(apps.events().into_iter().filter(|e| kind(e)));
            if !got.is_empty() {
                return got;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        got
    }

    /// Every taken or lost event, waiting up to a few seconds until `done`
    /// holds for them, in order.
    fn until(apps: &Apps, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < deadline && !done(&got) {
            got.extend(
                apps.events()
                    .into_iter()
                    .filter(|e| matches!(e, Event::Taken { .. } | Event::Lost { .. })),
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        got
    }

    fn lost_then_taken(events: &[Event]) -> bool {
        let lost = events.iter().position(|e| matches!(e, Event::Lost { .. }));
        lost.is_some_and(|at| events[at..].iter().any(|e| matches!(e, Event::Taken { .. })))
    }

    fn uuid_of(event: &Event) -> &str {
        match event {
            Event::Taken { uuid, .. } | Event::Lost { uuid, .. } | Event::Refused { uuid, .. } => uuid,
            Event::Listed(_) => "",
        }
    }

    /// An app wanted is tapped once it runs — its helper with it — and let
    /// go when its input goes.
    #[test]
    fn a_wanted_app_is_tapped_while_it_runs_and_let_go_after() {
        let fake = Fake::default();
        let mut apps = Apps::spawn(Box::new(fake.clone()));
        apps.want(vec![("in1".into(), "us.zoom.xos".into(), 0)]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(fake.takes.lock().unwrap().is_empty(), "not running: nothing tapped");

        // A helper is listed under its app (`app_sound::owning_app`); an
        // app whose ID merely extends Zoom's is another app.
        *fake.processes.lock().unwrap() = vec![
            (7, "us.zoom.xos".into(), true),
            (8, "us.zoom.xos".into(), false),
            (9, "com.apple.Music".into(), true),
            (10, "us.zoom.xos.beta".into(), true),
        ];
        let taken = next(&apps, |e| matches!(e, Event::Taken { .. }));
        assert_eq!(uuid_of(&taken[0]), "in1");
        assert_eq!(
            fake.takes.lock().unwrap().as_slice(),
            [("us.zoom.xos".to_string(), vec![7, 8])]
        );

        apps.want(Vec::new());
        let lost = next(&apps, |e| matches!(e, Event::Lost { .. }));
        assert_eq!(uuid_of(&lost[0]), "in1");
    }

    /// Restarted, an app has other processes: it is let go and tapped
    /// afresh. A failed device is tapped afresh too.
    #[test]
    fn a_restarted_app_or_a_failed_device_is_tapped_afresh() {
        let fake = Fake::default();
        *fake.processes.lock().unwrap() = vec![(7, "us.zoom.xos".into(), true)];
        let mut apps = Apps::spawn(Box::new(fake.clone()));
        apps.want(vec![("in1".into(), "us.zoom.xos".into(), 0)]);
        assert_eq!(next(&apps, |e| matches!(e, Event::Taken { .. })).len(), 1);

        *fake.processes.lock().unwrap() = vec![(12, "us.zoom.xos".into(), true)];
        let events = until(&apps, lost_then_taken);
        assert!(lost_then_taken(&events), "let go, then tapped again: {}", events.len());
        assert_eq!(fake.takes.lock().unwrap().last().unwrap().1, vec![12]);

        // A failed device: let go and, the app still running, tapped again.
        let takes = fake.takes.lock().unwrap().len();
        fake.fail.store(true, Ordering::Relaxed);
        let events = until(&apps, lost_then_taken);
        fake.fail.store(false, Ordering::Relaxed);
        assert!(lost_then_taken(&events), "tapped afresh: {}", events.len());
        assert!(fake.takes.lock().unwrap().len() > takes);
    }

    /// Two inputs asking for one refused app are each told; a refused app
    /// that quits is let go, so its strip says "not running" again.
    #[test]
    fn each_input_is_told_its_refusal_and_a_refused_app_quitting_is_let_go() {
        let fake = Fake::default();
        fake.refuse.store(true, Ordering::Relaxed);
        *fake.processes.lock().unwrap() = vec![(3, "us.zoom.xos".into(), true)];
        let mut apps = Apps::spawn(Box::new(fake.clone()));
        apps.want(vec![
            ("in1".into(), "us.zoom.xos".into(), 0),
            ("in2".into(), "us.zoom.xos".into(), 0),
        ]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut told: Vec<String> = Vec::new();
        while Instant::now() < deadline && told.len() < 2 {
            for e in apps.events() {
                if let Event::Refused { uuid, .. } = e {
                    told.push(uuid);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        told.sort();
        assert_eq!(told, ["in1", "in2"], "each input told");

        fake.processes.lock().unwrap().clear();
        let lost = until(&apps, |events| {
            events.iter().filter(|e| matches!(e, Event::Lost { .. })).count() >= 2
        });
        let mut gone: Vec<&str> = lost
            .iter()
            .filter(|e| matches!(e, Event::Lost { .. }))
            .map(uuid_of)
            .collect();
        gone.sort_unstable();
        assert_eq!(gone, ["in1", "in2"], "let go once not running");
    }

    /// An input refused for one app, its UUID then given to another refused
    /// for the same reason, is told again: the refusal is the app's, not the
    /// input's.
    #[test]
    fn an_inputs_refusal_is_told_again_for_another_app() {
        let fake = Fake::default();
        fake.refuse.store(true, Ordering::Relaxed);
        *fake.processes.lock().unwrap() = vec![
            (3, "us.zoom.xos".into(), true),
            (4, "com.microsoft.teams2".into(), true),
        ];
        let mut apps = Apps::spawn(Box::new(fake.clone()));
        let refused = |apps: &Apps, app: &str, generation: u64| {
            !next(apps, |e| {
                matches!(e, Event::Refused { uuid, app: a, generation: g, why }
                    if uuid == "in1" && a == app && *g == generation && why == "refused")
            })
            .is_empty()
        };
        apps.want(vec![("in1".into(), "us.zoom.xos".into(), 0)]);
        assert!(refused(&apps, "us.zoom.xos", 0));
        apps.want(vec![("in1".into(), "com.microsoft.teams2".into(), 1)]);
        assert!(refused(&apps, "com.microsoft.teams2", 1), "told again");
    }

    /// The list offered names each app once, making sound when any of its
    /// processes is, and is handed back when it changes; a refused tap is
    /// said so, and tried again later.
    #[test]
    fn the_apps_are_listed_and_a_refused_tap_tried_again() {
        let fake = Fake::default();
        fake.refuse.store(true, Ordering::Relaxed);
        *fake.processes.lock().unwrap() = vec![
            (3, "com.google.Chrome".into(), true),
            (4, "com.google.Chrome".into(), false),
        ];
        let mut apps = Apps::spawn(Box::new(fake.clone()));
        apps.want(vec![("in1".into(), "com.google.Chrome".into(), 0)]);
        // The list and the refusal come in one look: gather both.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen: Vec<Event> = Vec::new();
        let has = |seen: &[Event], listed: bool| {
            seen.iter().any(|e| {
                if listed {
                    matches!(e, Event::Listed(_))
                } else {
                    matches!(e, Event::Refused { .. })
                }
            })
        };
        while Instant::now() < deadline && !(has(&seen, true) && has(&seen, false)) {
            seen.extend(apps.events());
            std::thread::sleep(Duration::from_millis(20));
        }
        let Some(Event::Listed(list)) = seen.iter().find(|e| matches!(e, Event::Listed(_))) else {
            panic!("no list handed back")
        };
        assert_eq!(
            list.as_slice(),
            [RunningApp {
                bundle_id: "com.google.Chrome".into(),
                name: "com.google.Chrome app".into(),
                playing: true,
            }]
        );
        assert!(
            seen.iter().any(
                |e| matches!(e, Event::Refused { uuid, why, .. } if uuid == "in1" && why == "refused")
            ),
            "the refusal is said"
        );
        assert!(fake.takes.lock().unwrap().is_empty(), "refused");
        fake.refuse.store(false, Ordering::Relaxed);
        assert!(!next(&apps, |e| matches!(e, Event::Taken { .. })).is_empty());
    }
}
