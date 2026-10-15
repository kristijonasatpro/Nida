//! Every output frame's way to its screen, step by step, for a few seconds
//! on request (`TracePresents`), written to a file.
//!
//! An output's figures — frames presented, kept back, replaced, refreshes
//! missed — say how often a frame went wrong, not when or why. A frame goes
//! from the engine (kept back to its screen's rate, or handed to the window's
//! presenter, replacing one not yet taken) to the presenter (taken, a frame of
//! the window's to draw into acquired, the copy submitted, presented) to the
//! screen (Metal's `presentedTime`). With every step on one clock — the
//! host's, which Metal's own times are on — a frame that missed its refresh
//! shows which step was late; and the times a window's frames were shown give
//! its screen's refreshes, to set each step against.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

/// The longest trace asked for, seconds.
pub const LONGEST: f64 = 60.0;

/// One step of a frame's way to its screen, or what the engine and the GPU
/// did meanwhile.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// The engine started frame `number`, called for by the refresh at
    /// `refresh` (none when a timer starts frames).
    Frame { number: u64, refresh: Option<f64> },
    /// The control window took the engine's latest frame, `engine_frame`,
    /// to show under its UI.
    Sampled { engine_frame: u64 },
    /// The frame was drawn but kept back, to keep to its screen's rate.
    KeptBack,
    /// The frame was handed to the window's presenter.
    Handed,
    /// More frames waited for the presenter than may: the oldest was let go
    /// for the one handed over.
    Replaced,
    /// The presenter let the frame go unpresented, to catch up with its
    /// screen after a second a frame or more behind.
    LetGo,
    /// The presenter took the frame.
    Taken,
    /// A frame of the window's to draw into was had.
    Acquired,
    /// The window had no frame to draw into: the frame was not presented.
    NoFrame,
    /// The copy into the window's frame was submitted.
    Submitted,
    /// The frame was presented.
    Presented,
    /// When the window's frames presented since were shown, oldest first, 0
    /// for one never shown — as Metal reported them.
    Shown { times: Vec<f64> },
    /// A command buffer ran on the GPU.
    Gpu { label: String, start: f64, end: f64 },
    /// A live input's frame for the show frame due at `show_at`: captured at
    /// `captured` (host seconds), after `skipped` let go — or, `repeated`,
    /// none, the one before shown again. With the delay it was picked by and
    /// how many waited after.
    Picked {
        input: String,
        show_at: f64,
        captured: Option<f64>,
        skipped: usize,
        repeated: bool,
        delay: Option<f64>,
        waiting: usize,
    },
    /// A live input's frame captured at `captured` (host seconds) is ready to
    /// pick: the step's time less `captured` is how long it took to come. The
    /// capture called back at `called`, when known: before that the camera's
    /// and the system's way, after it Nida's turning it into a picture.
    /// `stamped`: `captured` is the frame's sample time (else when it came);
    /// `fresh`: its picture's memory was new.
    Arrived {
        input: String,
        captured: f64,
        called: Option<f64>,
        stamped: Option<bool>,
        fresh: Option<bool>,
    },
}

/// A step, when it happened (host seconds), whose window it was (none for
/// the engine's and the GPU's) and the window's number for the frame.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub at: f64,
    /// Shared, so a thread in real time notes its steps without copying it.
    #[serde(skip_serializing_if = "is_blank", serialize_with = "shared_str")]
    pub output: Arc<str>,
    #[serde(skip_serializing_if = "is_zero")]
    pub frame: u64,
    #[serde(flatten)]
    pub step: Step,
}

fn is_blank(output: &Arc<str>) -> bool {
    output.is_empty()
}

fn shared_str<S: serde::Serializer>(output: &Arc<str>, to: S) -> Result<S::Ok, S::Error> {
    to.serialize_str(output)
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What an output window was, for reading its steps.
#[derive(Clone, Debug, Serialize)]
pub struct OutputInfo {
    pub name: String,
    /// The display it is on (a `CGDirectDisplayID`).
    pub display: Option<u32>,
    /// Its screen's refresh interval, milliseconds.
    pub refresh_ms: Option<f64>,
    /// Whether its screen's refresh starts frames.
    pub follows_clock: bool,
    /// How long each frame is held on screen, seconds.
    pub frame_hold: Option<f64>,
}

#[derive(Serialize)]
struct Written<'a> {
    /// What starts frames, as the frame loop says.
    clock: &'a str,
    /// The show's frame time, milliseconds.
    frame_ms: f64,
    outputs: &'a [OutputInfo],
    events: &'a [Event],
}

struct Recording {
    until: f64,
    path: PathBuf,
    events: Vec<Event>,
}

/// A trace being made, or none.
#[derive(Default)]
pub struct PresentTrace {
    on: AtomicBool,
    recording: Mutex<Option<Recording>>,
}

/// The app's trace: the engine thread and every window's presenter note
/// their steps in it.
pub static TRACE: PresentTrace = PresentTrace::new();

impl PresentTrace {
    pub const fn new() -> Self {
        Self {
            on: AtomicBool::new(false),
            recording: Mutex::new(None),
        }
    }

    /// Trace for `secs` (at most [`LONGEST`]) from now, into a file at
    /// `path`; a trace being made is dropped.
    pub fn start(&self, secs: f64, path: PathBuf) {
        let until = host_seconds() + secs.clamp(0.0, LONGEST);
        // Room for a few seconds of every output's steps, so noting rarely
        // grows the list while holding the trace.
        let dropped = self.lock().replace(Recording {
            until,
            path,
            events: Vec::with_capacity(16_384),
        });
        self.on.store(true, Ordering::Release);
        // A trace's steps let go with the lock let go: a thread noting a
        // step meanwhile does not wait on it.
        drop(dropped);
    }

    /// Whether a trace is being made: nothing is noted otherwise.
    pub fn recording(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    /// Note `step` of frame `frame` of window `output`, now.
    pub fn note(&self, output: &str, frame: u64, step: Step) {
        if self.recording() {
            self.note_at(host_seconds(), output, frame, step);
        }
    }

    /// Note `step` of frame `frame` of `output`, now — or not at all, when
    /// the trace is busy being started or written: for threads running in
    /// real time, which must never wait on it, nor copy the name.
    pub fn note_if_free(&self, output: &Arc<str>, frame: u64, step: Step) {
        if !self.recording() {
            return;
        }
        let at = host_seconds();
        if let Ok(mut held) = self.recording.try_lock() {
            if let Some(recording) = held.as_mut() {
                recording.events.push(Event {
                    at,
                    output: Arc::clone(output),
                    frame,
                    step,
                });
            }
        }
    }

    /// Note `step` at host time `at`.
    pub fn note_at(&self, at: f64, output: &str, frame: u64, step: Step) {
        if !self.recording() {
            return;
        }
        if let Some(recording) = self.lock().as_mut() {
            recording.events.push(Event {
                at,
                output: Arc::from(output),
                frame,
                step,
            });
        }
    }

    /// When the trace's time is up: the trace ends, and is written with what
    /// the outputs were (`outputs`, asked for only then) on a thread of its
    /// own — sorted and written in the engine's frame, a 45 s trace of every
    /// output's and input's steps held a frame back 60 ms. The thread logs
    /// the file's path and how many steps it has, or why it could not be
    /// written, and gives the same back when waited for.
    pub fn finish_if_due(
        &self,
        clock: &str,
        frame_ms: f64,
        outputs: impl FnOnce() -> Vec<OutputInfo>,
    ) -> Option<std::thread::JoinHandle<std::io::Result<(PathBuf, usize)>>> {
        if !self.recording() {
            return None;
        }
        let recording = {
            let mut held = self.lock();
            if held.as_ref().is_none_or(|r| host_seconds() < r.until) {
                return None;
            }
            self.on.store(false, Ordering::Release);
            held.take()?
        };
        let clock = clock.to_string();
        let outputs = outputs();
        let spawned = std::thread::Builder::new()
            .name("present-trace".into())
            .spawn(move || {
                let mut events = recording.events;
                events.sort_by(|a, b| a.at.total_cmp(&b.at));
                let written = Written {
                    clock: &clock,
                    frame_ms,
                    outputs: &outputs,
                    events: &events,
                };
                let result =
                    write(&recording.path, &written).map(|()| (recording.path, events.len()));
                match &result {
                    Ok((path, steps)) => {
                        log::info!("Present trace written: {} ({steps} steps)", path.display());
                    }
                    Err(e) => log::warn!("Present trace not written: {e}"),
                }
                result
            });
        match spawned {
            Ok(writing) => Some(writing),
            Err(e) => {
                log::warn!("Present trace not written: no thread for it: {e}");
                None
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Recording>> {
        self.recording
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn write(path: &Path, written: &Written<'_>) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut out, written)?;
    std::io::Write::flush(&mut out)
}

/// Where a trace asked for now is written: beside Nida's log, or in the
/// temporary folder.
pub fn trace_path() -> PathBuf {
    let dir = std::env::var_os("VARDA_LOG_FILE")
        .map(PathBuf::from)
        .and_then(|log| log.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    dir.join(trace_name(stamp))
}

/// The file name of a trace asked for in second `stamp`: one more in the same
/// second is numbered — two traces' files, written each on a thread of its
/// own, must not be one.
fn trace_name(stamp: u64) -> String {
    static LAST: Mutex<(u64, u32)> = Mutex::new((0, 0));
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    let n = if last.0 == stamp { last.1 + 1 } else { 1 };
    *last = (stamp, n);
    if n == 1 {
        format!("present-trace-{stamp}.json")
    } else {
        format!("present-trace-{stamp}-{n}.json")
    }
}

pub use crate::host_clock::{host_seconds, host_seconds_of};

#[cfg(test)]
mod tests {
    use super::*;

    fn written(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).expect("written")).expect("JSON")
    }

    /// A thread in real time notes its step only when the trace is free: it
    /// never waits while the trace is being started or written.
    #[test]
    fn a_step_noted_in_real_time_never_waits_for_the_trace() {
        let dir = tempfile::tempdir().expect("a folder");
        let trace = std::sync::Arc::new(PresentTrace::new());
        trace.start(30.0, dir.path().join("trace.json"));
        let count = |t: &PresentTrace| t.lock().as_ref().map_or(0, |r| r.events.len());
        {
            let _busy = trace.lock();
            let theirs = std::sync::Arc::clone(&trace);
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                theirs.note_if_free(&Arc::from("Box"), 1, Step::Taken);
                let _ = done.send(());
            });
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("noted without waiting");
        }
        assert_eq!(count(&trace), 0, "passed over while busy");
        trace.note_if_free(&Arc::from("Box"), 2, Step::Taken);
        assert_eq!(count(&trace), 1, "noted when free");
    }

    /// Nothing is noted until a trace is asked for; then each step with its
    /// window and frame, in time order, and once its time is up the trace
    /// is written with what the outputs were, and noting stops.
    #[test]
    fn a_trace_notes_steps_while_asked_for_and_is_written_when_its_time_is_up() {
        let dir = tempfile::tempdir().expect("a folder");
        let path = dir.path().join("trace.json");
        let trace = PresentTrace::new();
        trace.note("TV", 1, Step::Handed);
        assert!(!trace.recording());

        trace.start(30.0, path.clone());
        let now = host_seconds();
        trace.note_at(now + 0.002, "TV", 7, Step::Presented);
        trace.note_at(now + 0.001, "TV", 7, Step::Handed);
        trace.note_at(
            now + 0.003,
            "",
            0,
            Step::Gpu {
                label: "Output 'TV' Swap Blit".into(),
                start: now + 0.0025,
                end: now + 0.003,
            },
        );
        let info = || {
            vec![OutputInfo {
                name: "TV".into(),
                display: Some(3),
                refresh_ms: Some(16.667),
                follows_clock: false,
                frame_hold: None,
            }]
        };
        assert!(
            trace.finish_if_due("display 2", 16.667, info).is_none(),
            "not yet"
        );

        // Its time up: started again for no time at all, keeping the steps.
        trace.lock().as_mut().expect("recording").until = now;
        let (at, steps) = trace
            .finish_if_due("display 2", 16.667, info)
            .expect("due")
            .join()
            .expect("the writing thread")
            .expect("written");
        assert_eq!((at.as_path(), steps), (path.as_path(), 3));
        assert!(!trace.recording());
        trace.note("TV", 8, Step::Taken);

        let file = written(&path);
        assert_eq!(file["clock"], "display 2");
        assert_eq!(file["outputs"][0]["name"], "TV");
        assert_eq!(file["outputs"][0]["follows_clock"], false);
        let events = file["events"].as_array().expect("events");
        let steps: Vec<_> = events.iter().map(|e| e["step"].as_str()).collect();
        assert_eq!(steps, [Some("handed"), Some("presented"), Some("gpu")]);
        assert_eq!(events[0]["output"], "TV");
        assert_eq!(events[0]["frame"], 7);
        assert!(events[2].get("output").is_none(), "the GPU's: no window");
        assert_eq!(events[2]["label"], "Output 'TV' Swap Blit");
    }

    /// Traces asked for in the same second go in files of their own: each is
    /// written on a thread of its own, and two writing one file at once could
    /// leave neither readable.
    #[test]
    fn traces_asked_for_in_one_second_get_files_of_their_own() {
        let stamp = 1_791_000_000;
        let names = [trace_name(stamp), trace_name(stamp), trace_name(stamp + 1)];
        assert_eq!(names[0], format!("present-trace-{stamp}.json"));
        assert_eq!(names[1], format!("present-trace-{stamp}-2.json"));
        assert_eq!(names[2], format!("present-trace-{}.json", stamp + 1));
    }
}
