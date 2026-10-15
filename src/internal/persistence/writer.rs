//! The show's files, written on a thread of their own.
//!
//! A save used to write `scene.json`, `stage.json` and the rest on the thread
//! that held the engine — the control window's for Save, the engine's own
//! for the API — so every output waited for the disk. On the operator's
//! laptop, its disk nearly full, one small file took half a second: its log
//! of 10 October 2026 has "Saved scene", then the control window's "The
//! engine thread kept the show over 100ms", then "Saved MIDI mappings" the
//! second after, and the TV, the LED wall and the stream stood still for
//! each save.
//!
//! Now the engine turns the show into text, which takes a millisecond, and
//! [`ShowWriter`] writes it: one save after another, in the order they were
//! made, telling the engine as each lands. Whatever reads the files back, or
//! writes them itself — a show opened, the save made as Nida quits — waits
//! for the saves before it ([`ShowWriter::flush`]).
//!
//! The rest the operator saves goes the same way, in the same line: a Save
//! As — the show's folder copied into the new one, presets, LUTs and shaders
//! too, then the show written there ([`ShowMove`]) — and a file of its own:
//! a preset, the Recent list, the saved layouts ([`FileSave`]).
//!
//! A Save As leaves the engine in the show it was saved from until the copy
//! has landed, so nothing reads a folder half made. A save the engine makes
//! meanwhile names the folder it is in; written after the copy, it goes into
//! the new folder instead, as the operator meant — the show saved from stays
//! as it was — until the engine has moved and names the new folder itself
//! ([`ShowWriter::settled`]). A Save As that fails moves nothing: the saves
//! after it go where they say.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// One file of a save: which it is, where it goes, and its text — or why
/// there is none.
pub struct ShowFile {
    /// What a failure is reported under: `scene`, `stage`…
    pub key: &'static str,
    /// What the log calls it: "scene", "MIDI mappings"…
    pub label: &'static str,
    pub path: PathBuf,
    pub content: Result<String, String>,
}

/// What to do once a save has landed, on the thread that wrote it.
pub type Then = Box<dyn FnOnce(&Landed) + Send>;

/// A save: one show's files, written in order into its folder.
pub struct ShowSave {
    /// The show's `.varda` folder, made first if it is not there.
    pub dir: PathBuf,
    pub files: Vec<ShowFile>,
    /// The show's name, and whether to say it is saved once it is.
    pub show: String,
    pub announce: bool,
    /// Told what came of it as soon as it lands — an HTTP request waiting
    /// for the files, say.
    pub then: Option<Then>,
}

/// What a Save As does before the show is written: makes sure the new
/// folder can take it and copies the show's folder in — or says why not.
pub type CopyShow = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// A Save As: the show's folder copied into a new one, then the show
/// written there.
pub struct ShowMove {
    /// The show's `.varda` folder: where the engine's saves go until it has
    /// moved.
    pub from: PathBuf,
    pub copy: CopyShow,
    /// The show, saved into the new folder (its `dir`) once the copy is made.
    pub save: ShowSave,
}

/// A file written whole, in line with the saves: a preset, the Recent list…
pub struct FileSave {
    /// What the log calls it: "deck preset 'Intro'".
    pub label: String,
    pub path: PathBuf,
    pub content: String,
    /// Said once it has landed — "💾 Saved deck preset 'Intro'" — if
    /// anything.
    pub said: Option<String>,
    /// How a failure is put to the operator — "Failed to save preset" — or
    /// none, for a file only the log need hear of.
    pub failed: Option<String>,
    /// A preset: the presets folder it lands in is read again once it has,
    /// on this thread, and comes with its [`Landed`].
    pub presets: bool,
    pub then: Option<Then>,
}

/// What came of a save.
#[derive(Debug)]
pub struct Landed {
    /// Which it was, as handing it over answered.
    pub id: u64,
    /// What to tell the operator, all of it having landed.
    pub said: Option<String>,
    /// Each part that did not land, and why: `"stage: …"`.
    pub errors: Vec<String>,
    /// How a failure is put to the operator, if it is.
    failed: Option<String>,
    /// The file a file of its own was written to — where a Save As moved
    /// it, if it did; none for a show's save.
    pub path: Option<PathBuf>,
    /// For a preset, the presets folder it landed in, read again then —
    /// whatever came of it (see `PresetLibrary::settle`).
    pub presets: Option<super::presets::PresetLibrary>,
}

impl Landed {
    /// What to tell the operator when something did not land.
    pub fn error_message(&self) -> Option<String> {
        let failed = self.failed.as_deref()?;
        (!self.errors.is_empty()).then(|| format!("{failed}: {}", self.errors.join("; ")))
    }
}

/// How a show's save that fails is put.
const SHOW_FAILED: &str = "Failed to save workspace";

/// Why a save that panicked did not land.
const BROKE: &str = "the save stopped half way";

/// Write a save's files now, on this thread.
pub fn write(save: ShowSave) -> Landed {
    write_as(0, save)
}

fn write_as(id: u64, save: ShowSave) -> Landed {
    let started = Instant::now();
    if let Some(wait) = slow_disk_for_test() {
        std::thread::sleep(wait);
    }
    let mut errors = Vec::new();
    let made = !save.dir.is_dir();
    if let Err(e) = std::fs::create_dir_all(&save.dir) {
        log::error!("Failed to create {}: {e}", save.dir.display());
        errors.push(format!("cannot make {}: {e}", save.dir.display()));
    } else {
        if made {
            log::info!("Created workspace directory: {}", save.dir.display());
        }
        for file in &save.files {
            let written = match &file.content {
                Ok(text) => super::atomic_write(&file.path, text),
                Err(why) => Err(anyhow::anyhow!("{why}")),
            };
            match written {
                Ok(()) => log::info!("Saved {} to {}", file.label, file.path.display()),
                Err(e) => {
                    log::error!("Failed to save {}: {e}", file.label);
                    errors.push(format!("{}: {e}", file.key));
                }
            }
        }
    }
    let took = started.elapsed();
    if took > Duration::from_millis(100) {
        log::warn!(
            "Writing “{}” took {} ms: the disk is slow",
            save.show,
            took.as_millis()
        );
    }
    let landed = Landed {
        id,
        said: save.announce.then(|| format!("💾 “{}” saved", save.show)),
        errors,
        failed: Some(SHOW_FAILED.into()),
        path: None,
        presets: None,
    };
    if let Some(then) = save.then {
        then(&landed);
    }
    landed
}

/// A Save As, now, on this thread: the copy, then the show written there.
fn move_show(id: u64, show: ShowMove) -> Landed {
    let ShowMove { from, copy, save } = show;
    if let Err(why) = copy() {
        log::error!(
            "The show was not copied from {} to {}: {why}",
            from.display(),
            save.dir.display()
        );
        let landed = Landed {
            id,
            said: None,
            errors: vec![why],
            failed: Some(SHOW_FAILED.into()),
            path: None,
            presets: None,
        };
        if let Some(then) = save.then {
            then(&landed);
        }
        return landed;
    }
    log::info!(
        "Copied the show from {} to {}",
        from.display(),
        save.dir.display()
    );
    write_as(id, save)
}

/// A file of its own, now, on this thread: its folder made if need be,
/// then the file written whole.
fn write_file(id: u64, file: FileSave) -> Landed {
    if let Some(wait) = slow_disk_for_test() {
        std::thread::sleep(wait);
    }
    let written = match file.path.parent() {
        Some(dir) => std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("cannot make {}: {e}", dir.display())),
        None => Ok(()),
    }
    .and_then(|()| super::atomic_write(&file.path, &file.content));
    let errors = match written {
        Ok(()) => {
            log::info!("Saved {} to {}", file.label, file.path.display());
            Vec::new()
        }
        Err(e) => {
            log::error!("Failed to save {}: {e:#}", file.label);
            vec![format!("{e:#}")]
        }
    };
    let landed = Landed {
        id,
        said: file.said,
        errors,
        failed: file.failed,
        presets: file.presets.then(|| presets_by(&file.path)).flatten(),
        path: Some(file.path),
    };
    if let Some(then) = file.then {
        then(&landed);
    }
    landed
}

/// The presets in the folder the preset `path` is in, as a scan reads them:
/// `…/presets/decks/Logo.json` → `…/presets`.
fn presets_by(path: &Path) -> Option<super::presets::PresetLibrary> {
    let presets = path.parent()?.parent()?;
    Some(super::presets::PresetLibrary::load_from(presets))
}

/// A file not written, as a disk refusing it would leave it — for tests.
#[cfg(test)]
fn refuse_file(id: u64, file: FileSave) -> Landed {
    let landed = Landed {
        id,
        said: file.said,
        errors: vec!["refused for a test".into()],
        failed: file.failed,
        presets: file.presets.then(|| presets_by(&file.path)).flatten(),
        path: Some(file.path),
    };
    if let Some(then) = file.then {
        then(&landed);
    }
    landed
}

/// A test hook: every save waits this long before writing, as on the
/// laptop whose disk was nearly full — `NIDA_TEST_SLOW_SAVE=600` for 600 ms.
fn slow_disk_for_test() -> Option<Duration> {
    static SLOW: OnceLock<Option<Duration>> = OnceLock::new();
    *SLOW.get_or_init(|| {
        let ms: u64 = std::env::var("NIDA_TEST_SLOW_SAVE")
            .ok()?
            .trim()
            .parse()
            .ok()
            .filter(|ms| *ms > 0)?;
        log::warn!("Test hook: every save waits {ms} ms before writing, as a slow disk would");
        Some(Duration::from_millis(ms))
    })
}

/// What is handed over to be written.
enum Work {
    Save(ShowSave),
    Move(ShowMove),
    File(FileSave),
}

impl Work {
    /// What is to be told once it has landed, taken out to be told after —
    /// whatever came of it.
    fn take_then(&mut self) -> Option<Then> {
        match self {
            Self::Save(save) => save.then.take(),
            Self::Move(show) => show.save.then.take(),
            Self::File(file) => file.then.take(),
        }
    }

    /// How a failure is put to the operator, should it stop half way.
    fn failed(&self) -> Option<String> {
        match self {
            Self::Save(_) | Self::Move(_) => Some(SHOW_FAILED.into()),
            Self::File(file) => file.failed.clone(),
        }
    }
}

/// What the thread is handed: work with its number; word that the engine
/// has moved with the show ([`ShowWriter::settled`]); or a mark to say when
/// it is reached — everything before it has landed by then.
enum Job {
    Do(u64, Work),
    Settled,
    Mark(mpsc::Sender<()>),
}

/// Where a Save As that landed moved the show: its `.varda` folder before
/// and after, and its new name.
#[derive(Clone)]
struct Moved {
    from: PathBuf,
    to: PathBuf,
    show: String,
}

/// What the writer keeps from one piece of work to the next: where a Save
/// As moved the show, until the engine names the new folder itself. Shared
/// by the thread and the work done here without it, so a thread started
/// late, or gone, does not forget it.
#[derive(Default)]
struct Desk {
    moved: Option<Moved>,
    /// Files of their own to refuse, as a disk would — for tests.
    #[cfg(test)]
    refused_files: u32,
    /// Files of their own whose writing panics — for tests.
    #[cfg(test)]
    breaking_files: u32,
}

/// The desk, even after a save panicked holding it.
fn at(desk: &Mutex<Desk>) -> MutexGuard<'_, Desk> {
    desk.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Desk {
    fn run(&mut self, id: u64, work: Work) -> Landed {
        match work {
            Work::Save(save) => write_as(id, self.moved_save(save)),
            Work::Move(show) => {
                let moved = Moved {
                    from: show.from.clone(),
                    to: show.save.dir.clone(),
                    show: show.save.show.clone(),
                };
                let landed = move_show(id, show);
                if landed.errors.is_empty() {
                    self.moved = Some(moved);
                }
                landed
            }
            Work::File(mut file) => {
                if let Some(path) = self.moved_path(&file.path) {
                    file.path = path;
                }
                #[cfg(test)]
                if self.refused_files > 0 {
                    self.refused_files -= 1;
                    return refuse_file(id, file);
                }
                #[cfg(test)]
                if self.breaking_files > 0 {
                    self.breaking_files -= 1;
                    panic!("a file that breaks, for a test");
                }
                write_file(id, file)
            }
        }
    }

    /// `path` in the folder a Save As moved the show to, if it is in the
    /// one the show left.
    fn moved_path(&self, path: &Path) -> Option<PathBuf> {
        let moved = self.moved.as_ref()?;
        let rest = path.strip_prefix(&moved.from).ok()?;
        Some(moved.to.join(rest))
    }

    /// A save the engine made of the show before it heard the show had
    /// moved, written where the show went.
    fn moved_save(&self, mut save: ShowSave) -> ShowSave {
        let Some(moved) = &self.moved else {
            return save;
        };
        if save.dir != moved.from {
            return save;
        }
        log::info!(
            "A save made as the show was saved as “{}” goes into {}",
            moved.show,
            moved.to.display()
        );
        for file in &mut save.files {
            if let Some(path) = self.moved_path(&file.path) {
                file.path = path;
            }
        }
        save.dir.clone_from(&moved.to);
        save.show.clone_from(&moved.show);
        save
    }
}

/// Saves written one after another on a thread of their own, started with
/// the first.
#[derive(Default)]
pub struct ShowWriter {
    worker: Option<Worker>,
    desk: Arc<Mutex<Desk>>,
    /// What landed without the thread.
    landed_here: Vec<Landed>,
    /// The number the last piece of work handed over was given.
    last_id: u64,
    /// Milliseconds each save waits on the thread before writing — a disk
    /// as slow as the laptop's, for tests (see [`ShowWriter::slow_for_test`]).
    slow_ms: Arc<AtomicU64>,
    /// Times the thread is not to start, as when the system is out of
    /// threads — for tests.
    #[cfg(test)]
    refused_starts: u32,
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    landed: mpsc::Receiver<Landed>,
    thread: std::thread::JoinHandle<()>,
}

impl Worker {
    fn start(slow_ms: Arc<AtomicU64>, desk: Arc<Mutex<Desk>>) -> std::io::Result<Self> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (tell, landed) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("show-writer".into())
            .spawn(move || {
                for job in inbox {
                    let (id, work) = match job {
                        Job::Do(id, work) => (id, work),
                        Job::Settled => {
                            at(&desk).moved = None;
                            continue;
                        }
                        Job::Mark(reached) => {
                            let _ = reached.send(());
                            continue;
                        }
                    };
                    let wait = slow_ms.load(Ordering::Relaxed);
                    if wait > 0 {
                        std::thread::sleep(Duration::from_millis(wait));
                    }
                    // Told once it has landed, whatever came of it — a save
                    // that broke included.
                    let mut work = work;
                    let then = work.take_then();
                    let failed = work.failed();
                    // A Save As whose callback breaks has not landed: the
                    // saves after it go where they say.
                    let moved_before =
                        matches!(work, Work::Move(_)).then(|| at(&desk).moved.clone());
                    // A preset that breaks still has its folder read again, where
                    // it was to land.
                    let preset = match &work {
                        Work::File(file) if file.presets => Some(
                            at(&desk)
                                .moved_path(&file.path)
                                .unwrap_or_else(|| file.path.clone()),
                        ),
                        _ => None,
                    };
                    // A save that panics still lands, as a failure, and the
                    // saves after it are written.
                    let landed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        at(&desk).run(id, work)
                    }))
                    .unwrap_or_else(|_| Landed {
                        id,
                        said: None,
                        errors: vec![BROKE.into()],
                        failed,
                        presets: preset.as_deref().and_then(|path| {
                            std::panic::catch_unwind(|| presets_by(path)).ok().flatten()
                        }),
                        path: preset,
                    });
                    // One that panics is the save's own failure.
                    let landed = match then {
                        Some(then) => {
                            let told =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    then(&landed);
                                }));
                            if told.is_ok() {
                                landed
                            } else {
                                if let Some(before) = moved_before {
                                    at(&desk).moved = before;
                                }
                                Landed {
                                    said: None,
                                    errors: vec![BROKE.into()],
                                    ..landed
                                }
                            }
                        }
                        None => landed,
                    };
                    let _ = tell.send(landed);
                }
            })?;
        Ok(Self {
            jobs,
            landed,
            thread,
        })
    }
}

impl ShowWriter {
    /// Hand a save over to be written after those before it. Returns the
    /// number its [`Landed`] will carry.
    pub fn submit(&mut self, save: ShowSave) -> u64 {
        self.hand_over(Work::Save(save))
    }

    /// Hand a Save As over: the show's folder copied, then the show written
    /// into the copy, after the saves before it.
    pub fn submit_move(&mut self, show: ShowMove) -> u64 {
        self.hand_over(Work::Move(show))
    }

    /// Hand a file of its own over, to be written after the saves before it.
    pub fn submit_file(&mut self, file: FileSave) -> u64 {
        self.hand_over(Work::File(file))
    }

    /// Should the thread not start — the system out of threads — the work
    /// is done here and now, the outputs waiting for it as they once did: a
    /// save lost would be worse than a frame held.
    fn hand_over(&mut self, work: Work) -> u64 {
        self.last_id += 1;
        let id = self.last_id;
        if self.worker.is_none() {
            match self.start() {
                Ok(worker) => self.worker = Some(worker),
                Err(e) => log::error!("No thread to write the show on, writing it here: {e}"),
            }
        }
        let work = match &self.worker {
            Some(worker) => match worker.jobs.send(Job::Do(id, work)) {
                Ok(()) => return id,
                Err(mpsc::SendError(Job::Do(_, work))) => work,
                Err(mpsc::SendError(_)) => unreachable!("work was sent"),
            },
            None => work,
        };
        let mut work = work;
        let then = work.take_then();
        let landed = at(&self.desk).run(id, work);
        if let Some(then) = then {
            then(&landed);
        }
        self.landed_here.push(landed);
        id
    }

    fn start(&mut self) -> std::io::Result<Worker> {
        #[cfg(test)]
        if self.refused_starts > 0 {
            self.refused_starts -= 1;
            return Err(std::io::Error::other("refused for a test"));
        }
        Worker::start(Arc::clone(&self.slow_ms), Arc::clone(&self.desk))
    }

    /// The engine has moved with the show a Save As copied: the saves it
    /// hands over from now name the new folder themselves, and go where
    /// they say.
    pub fn settled(&mut self) {
        // In line after the work handed over before it, which may still
        // name the old folder; with no thread, that work is done already.
        let queued = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.jobs.send(Job::Settled).is_ok());
        if !queued {
            at(&self.desk).moved = None;
        }
    }

    /// The saves that have landed since this was last asked, oldest first.
    pub fn landed(&mut self) -> Vec<Landed> {
        let mut landed = std::mem::take(&mut self.landed_here);
        if let Some(worker) = &self.worker {
            landed.extend(worker.landed.try_iter());
        }
        landed
    }

    /// Wait for every save handed over to land.
    pub fn flush(&self) {
        let Some(worker) = &self.worker else {
            return;
        };
        let (reached, mark) = mpsc::channel();
        if worker.jobs.send(Job::Mark(reached)).is_ok() {
            // An error: the thread has gone, and nothing is left to wait for.
            let _ = mark.recv();
        }
    }

    /// Each save handed over from now waits `wait` before it is written,
    /// as on a slow disk — for tests. A save written here and now
    /// ([`write`]) does not.
    pub fn slow_for_test(&self, wait: Duration) {
        let ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        self.slow_ms.store(ms, Ordering::Relaxed);
    }

    /// The thread does not start the next `starts` times it is asked to.
    #[cfg(test)]
    fn refuse_threads_for_test(&mut self, starts: u32) {
        self.refused_starts = starts;
    }

    /// The next `files` files of their own handed over are not written, as
    /// a disk refusing them would leave them.
    #[cfg(test)]
    pub(crate) fn refuse_files_for_test(&self, files: u32) {
        at(&self.desk).refused_files = files;
    }

    /// Writing the next `files` files of their own handed over panics.
    #[cfg(test)]
    pub(crate) fn break_files_for_test(&self, files: u32) {
        at(&self.desk).breaking_files = files;
    }
}

impl Drop for ShowWriter {
    /// Saves still on their way land before Nida lets go of the show.
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            drop(worker.jobs);
            let _ = worker.thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    fn save_of(dir: &std::path::Path, text: &str) -> ShowSave {
        ShowSave {
            dir: dir.join(".varda"),
            files: vec![
                ShowFile {
                    key: "scene",
                    label: "scene",
                    path: dir.join(".varda/scene.json"),
                    content: Ok(text.to_string()),
                },
                ShowFile {
                    key: "stage",
                    label: "stage",
                    path: dir.join(".varda/stage.json"),
                    content: Ok(format!("{text} stage")),
                },
            ],
            show: "Show".into(),
            announce: true,
            then: None,
        }
    }

    fn read(dir: &std::path::Path, file: &str) -> String {
        std::fs::read_to_string(dir.join(".varda").join(file)).unwrap()
    }

    /// A Save As from `from` into `to`: the copy a plain one, as the app's.
    fn move_of(from: &std::path::Path, to: &std::path::Path, text: &str) -> ShowMove {
        let (source, target) = (from.join(".varda"), to.join(".varda"));
        let mut save = save_of(to, text);
        save.show = "Moved".into();
        save.announce = false;
        ShowMove {
            from: source.clone(),
            copy: Box::new(move || copy_tree(&source, &target).map_err(|e| e.to_string())),
            save,
        }
    }

    /// A folder copied, as the app's Save As does: one never saved is
    /// nothing to copy.
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        let entries = match std::fs::read_dir(from) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), target)?;
            }
        }
        Ok(())
    }

    fn preset_in(dir: &std::path::Path, text: &str) -> FileSave {
        FileSave {
            label: "deck preset 'Logo'".into(),
            path: dir.join(".varda/presets/decks/Logo.json"),
            content: text.into(),
            said: Some("💾 Saved deck preset 'Logo'".into()),
            failed: Some("Failed to save preset".into()),
            presets: false,
            then: None,
        }
    }

    #[test]
    fn saves_land_in_the_order_they_were_made() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        for n in 0..20 {
            writer.submit(save_of(tmp.path(), &format!("save {n}")));
        }
        writer.flush();
        assert_eq!(read(tmp.path(), "scene.json"), "save 19");
        assert_eq!(read(tmp.path(), "stage.json"), "save 19 stage");
        let landed = writer.landed();
        assert_eq!(landed.len(), 20);
        assert!(landed
            .iter()
            .all(|l| l.errors.is_empty() && l.said.as_deref() == Some("💾 “Show” saved")));
        assert!(writer.landed().is_empty(), "each is told once");
    }

    #[test]
    fn a_save_is_written_off_the_thread_that_hands_it_over() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        let (release, stuck) = mpsc::channel::<()>();
        let reached = Arc::new(AtomicBool::new(false));
        let mut save = save_of(tmp.path(), "held");
        // The first file waits in the writer for the test to let it go: a
        // disk that does not answer. Handing the save over must not wait.
        let seen = Arc::clone(&reached);
        save.then = Some(Box::new(move |_| {
            seen.store(true, Ordering::SeqCst);
        }));
        let stall = ShowSave {
            dir: tmp.path().join(".varda"),
            files: Vec::new(),
            show: "Stall".into(),
            announce: false,
            then: Some(Box::new(move |_| {
                let _ = stuck.recv();
            })),
        };
        let t = Instant::now();
        writer.submit(stall);
        writer.submit(save);
        assert!(t.elapsed() < Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            !reached.load(Ordering::SeqCst),
            "the second waits for the first"
        );
        drop(release);
        writer.flush();
        assert!(reached.load(Ordering::SeqCst));
        assert_eq!(read(tmp.path(), "scene.json"), "held");
    }

    #[test]
    fn a_file_that_cannot_be_written_says_which_and_the_rest_land() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        let mut save = save_of(tmp.path(), "fine");
        save.files[0].content = Err("Failed to serialize scene".into());
        let answer = Arc::new(Mutex::new(None));
        let told = Arc::clone(&answer);
        save.then = Some(Box::new(move |landed| {
            *told.lock().unwrap() = landed.error_message();
        }));
        writer.submit(save);
        writer.flush();
        let landed = writer.landed();
        assert_eq!(
            landed[0].errors,
            vec!["scene: Failed to serialize scene".to_string()]
        );
        assert_eq!(
            answer.lock().unwrap().as_deref(),
            Some("Failed to save workspace: scene: Failed to serialize scene")
        );
        assert_eq!(read(tmp.path(), "stage.json"), "fine stage");
    }

    #[test]
    fn a_folder_that_cannot_be_made_is_reported() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blocker = tmp.path().join("not-a-folder");
        std::fs::write(&blocker, "a file").unwrap();
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&blocker, "x"));
        writer.flush();
        let landed = writer.landed();
        assert_eq!(landed.len(), 1);
        assert!(landed[0].error_message().unwrap().contains("cannot make"));
    }

    #[test]
    fn a_save_that_panics_lands_as_a_failure_and_the_next_is_written() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        let mut breaks = save_of(tmp.path(), "breaks");
        breaks.then = Some(Box::new(|_| panic!("a save that breaks")));
        writer.submit(breaks);
        writer.submit(save_of(tmp.path(), "next"));
        writer.flush();
        let landed = writer.landed();
        assert_eq!(landed.len(), 2);
        assert_eq!(
            landed[0].errors,
            vec!["the save stopped half way".to_string()]
        );
        assert!(landed[1].errors.is_empty());
        assert_eq!(read(tmp.path(), "scene.json"), "next");
    }

    #[test]
    fn saves_on_their_way_land_when_the_writer_is_let_go() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        for n in 0..5 {
            writer.submit(save_of(tmp.path(), &format!("last {n}")));
        }
        drop(writer);
        assert_eq!(read(tmp.path(), "scene.json"), "last 4");
    }

    #[test]
    fn each_landing_carries_the_number_its_handing_over_answered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        let first = writer.submit(save_of(tmp.path(), "a"));
        let second = writer.submit_file(preset_in(tmp.path(), "{}"));
        assert_ne!(first, second);
        writer.flush();
        let ids: Vec<u64> = writer.landed().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![first, second]);
    }

    #[test]
    fn a_file_of_its_own_lands_in_a_folder_made_for_it_and_says_so() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        writer.submit_file(preset_in(tmp.path(), "{\"a\":1}"));
        writer.flush();
        assert_eq!(read(tmp.path(), "presets/decks/Logo.json"), "{\"a\":1}");
        let landed = writer.landed();
        assert_eq!(landed[0].error_message(), None);
        assert_eq!(
            landed[0].said.as_deref(),
            Some("💾 Saved deck preset 'Logo'")
        );
    }

    #[test]
    fn a_file_that_cannot_land_is_put_as_its_own_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".varda/presets")).unwrap();
        // A file where its folder should be.
        std::fs::write(tmp.path().join(".varda/presets/decks"), "").unwrap();
        let mut writer = ShowWriter::default();
        writer.submit_file(preset_in(tmp.path(), "{}"));
        let mut quiet = preset_in(tmp.path(), "{}");
        quiet.failed = None;
        writer.submit_file(quiet);
        writer.flush();
        let landed = writer.landed();
        let said = landed[0].error_message().unwrap();
        assert!(
            said.starts_with("Failed to save preset: cannot make"),
            "{said}"
        );
        assert!(!landed[1].errors.is_empty());
        assert_eq!(landed[1].error_message(), None, "only the log hears");
    }

    /// A Save As copies the show's folder — its presets with it — and writes
    /// the show into the copy; the folder it left keeps what it had.
    #[test]
    fn a_save_as_copies_the_folder_then_writes_the_show_there() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&a, "a"));
        writer.submit_file(preset_in(&a, "logo"));
        writer.submit_move(move_of(&a, &b, "b"));
        writer.flush();
        assert_eq!(read(&b, "scene.json"), "b");
        assert_eq!(read(&b, "presets/decks/Logo.json"), "logo");
        assert_eq!(read(&a, "scene.json"), "a");
        assert!(writer.landed().iter().all(|l| l.errors.is_empty()));
    }

    /// The engine's saves name the show's old folder until it has heard
    /// the Save As landed: written after it, they go where the show went,
    /// under its new name — and once the engine has moved, where they say.
    #[test]
    fn saves_made_as_the_show_is_saved_as_go_where_it_went() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&a, "a"));
        writer.submit_move(move_of(&a, &b, "b"));
        writer.submit(save_of(&a, "made meanwhile"));
        writer.submit_file(preset_in(&a, "saved meanwhile"));
        writer.flush();
        assert_eq!(read(&b, "scene.json"), "made meanwhile");
        assert_eq!(read(&b, "presets/decks/Logo.json"), "saved meanwhile");
        assert_eq!(read(&a, "scene.json"), "a", "the show it left stays");
        assert!(!a.join(".varda/presets").exists());
        let landed = writer.landed();
        assert_eq!(landed[2].said.as_deref(), Some("💾 “Moved” saved"));
        assert_eq!(
            landed[3].path.as_deref(),
            Some(b.join(".varda/presets/decks/Logo.json").as_path()),
            "where it went"
        );

        writer.settled();
        writer.submit(save_of(&a, "A again"));
        writer.flush();
        assert_eq!(read(&a, "scene.json"), "A again");
        assert_eq!(read(&b, "scene.json"), "made meanwhile");
    }

    /// A file whose writing breaks still tells what came of it — those
    /// waiting on it are not left waiting.
    #[test]
    fn a_file_that_breaks_still_tells_what_came_of_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut writer = ShowWriter::default();
        writer.break_files_for_test(1);
        let told = Arc::new(Mutex::new(None));
        let mut breaks = preset_in(tmp.path(), "{}");
        let seen = Arc::clone(&told);
        breaks.then = Some(Box::new(move |landed| {
            *seen.lock().unwrap() = Some(landed.errors.clone());
        }));
        writer.submit_file(breaks);
        writer.submit_file(preset_in(tmp.path(), "next"));
        writer.flush();
        assert_eq!(
            told.lock().unwrap().clone(),
            Some(vec!["the save stopped half way".to_string()])
        );
        assert_eq!(read(tmp.path(), "presets/decks/Logo.json"), "next");
    }

    /// A Save As whose callback breaks has not landed: the saves after it
    /// go to the show it never left.
    #[test]
    fn a_save_as_whose_callback_breaks_moves_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        let mut breaks = move_of(&a, &b, "b");
        breaks.save.then = Some(Box::new(|_| panic!("a callback that breaks")));
        writer.submit_move(breaks);
        writer.submit(save_of(&a, "after"));
        writer.flush();
        assert_eq!(read(&a, "scene.json"), "after");
        let landed = writer.landed();
        assert_eq!(landed[0].errors, vec!["the save stopped half way"]);
    }

    /// A Save As written here, the thread not starting, and the thread
    /// starting for the next save: that save still goes where the show went.
    #[test]
    fn a_thread_started_late_remembers_where_the_show_went() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        writer.refuse_threads_for_test(1);
        writer.submit_move(move_of(&a, &b, "b"));
        assert!(writer.worker.is_none(), "written here");
        writer.submit(save_of(&a, "made meanwhile"));
        assert!(writer.worker.is_some(), "the thread started");
        writer.flush();
        assert_eq!(read(&b, "scene.json"), "made meanwhile");
        assert!(!a.exists());
    }

    /// A Save As whose copy lands but whose show cannot be written there
    /// moves nothing either.
    #[test]
    fn a_save_as_whose_files_fail_moves_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&a, "a"));
        let mut half = move_of(&a, &b, "b");
        half.save.files[1].content = Err("Failed to serialize stage".into());
        writer.submit_move(half);
        writer.submit(save_of(&a, "after"));
        writer.flush();
        assert_eq!(read(&a, "scene.json"), "after");
        assert_eq!(read(&b, "scene.json"), "b", "what was written stays");
        let landed = writer.landed();
        assert!(!landed[1].errors.is_empty());
    }

    /// Only what names the folder the show left follows it: another show,
    /// or a folder beside `.varda` whose name only starts the same, does not.
    #[test]
    fn only_what_was_in_the_show_left_follows_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            tmp.path().join("A"),
            tmp.path().join("B"),
            tmp.path().join("C"),
        );
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&a, "a"));
        writer.submit_move(move_of(&a, &b, "b"));
        writer.submit(save_of(&c, "c"));
        let mut beside = preset_in(&a, "beside");
        beside.path = a.join(".varda2/Logo.json");
        writer.submit_file(beside);
        writer.flush();
        assert_eq!(read(&c, "scene.json"), "c");
        assert_eq!(
            std::fs::read_to_string(a.join(".varda2/Logo.json")).unwrap(),
            "beside"
        );
        assert!(!b.join(".varda2").exists());
    }

    /// Word that the engine has moved comes in line: a save handed over
    /// before it, still waiting when the engine hears the move landed, goes
    /// where the show went.
    #[test]
    fn saves_waiting_when_the_engine_moves_still_follow_the_show() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        let moved = writer.submit_move(move_of(&a, &b, "b"));
        // The thread busy a while after the move: the save behind it waits.
        writer.submit(ShowSave {
            dir: tmp.path().join("stall"),
            files: Vec::new(),
            show: "Stall".into(),
            announce: false,
            then: Some(Box::new(|_| std::thread::sleep(Duration::from_millis(300)))),
        });
        writer.submit(save_of(&a, "waiting"));
        let until = Instant::now() + Duration::from_secs(10);
        while !writer.landed().iter().any(|l| l.id == moved) {
            assert!(Instant::now() < until, "the move never landed");
            std::thread::sleep(Duration::from_millis(2));
        }
        writer.settled();
        writer.submit(save_of(&a, "A again"));
        writer.flush();
        assert_eq!(read(&b, "scene.json"), "waiting");
        assert_eq!(read(&a, "scene.json"), "A again");
    }

    /// A Save As whose copy fails writes nothing into the new folder, and
    /// the saves after it go to the show it never left.
    #[test]
    fn a_save_as_that_cannot_copy_moves_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
        let mut writer = ShowWriter::default();
        writer.submit(save_of(&a, "a"));
        let mut refused = move_of(&a, &b, "b");
        refused.copy = Box::new(|| Err("B already holds other files".into()));
        writer.submit_move(refused);
        writer.submit(save_of(&a, "after"));
        writer.flush();
        assert!(!b.exists());
        assert_eq!(read(&a, "scene.json"), "after");
        let landed = writer.landed();
        assert_eq!(landed[1].errors, vec!["B already holds other files"]);
        assert!(landed[2].errors.is_empty());
    }
}
