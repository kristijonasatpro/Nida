//! What the show asks of files — whether one is there, its size and when it
//! last changed, a folder's files, a PDF's pages — answered at once from
//! what a thread of its own last found.
//!
//! Asked on a show thread, the disk can keep that thread waiting: a talk on
//! a USB stick pulled out, a network share gone, the system short of memory
//! (a test Nida sat 15 hours in a write to its log, 2026-11-02). The show's
//! own state asked it many times a second — every talk's file, every
//! frame's presenter view, the LUT folder in every control-window frame.
//! Asked here, a show thread gets the last answer, or none yet the first
//! time, and a thread looks again once an answer is [`FRESH`] old. Each
//! disk has its thread (see [`place_of`]): a network share gone keeps its
//! own thread waiting, while the Mac's own disk and the others are still
//! looked at.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// How long a file's or a folder's answer stands before it is looked for
/// again. A PDF's pages are counted once for each size and change time.
pub const FRESH: Duration = Duration::from_secs(1);

/// Answers asked for by no one in this long are let go.
const FORGOTTEN: Duration = Duration::from_secs(300);

/// How often the answers no one asks for are looked for, even while every
/// thread waits on its disk.
const SWEEP: Duration = Duration::from_secs(30);

/// Answers kept, at most: past it, those not asked for in a minute go — at
/// a sweep every second rather than every [`SWEEP`].
const KEPT_AT_MOST: usize = 4096;

/// A disk's thread with no question in this long ends; another starts
/// should one come.
const IDLE: Duration = Duration::from_secs(600);

/// Questions waiting for a disk's thread, at most; past that, a question is
/// put again when next asked.
const WAITING: usize = 1024;

/// A file as found: its size, when it last changed, and where it really is
/// (links followed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub real: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Question {
    File(PathBuf),
    Folder(PathBuf),
    /// The PDF at a path, as it was at this size and change time.
    PdfPages(PathBuf, u64, Option<SystemTime>),
}

impl Question {
    /// Whether `answer`, found at `found`, is to be looked for again `now`.
    /// A PDF counted stands for as long as it is the same file; one that
    /// did not open is tried again, as a file is looked at again.
    fn stale(&self, answer: Option<&Answer>, found: Option<Instant>, now: Instant) -> bool {
        let old = found.is_none_or(|at| now.duration_since(at) >= FRESH);
        match self {
            Self::File(_) | Self::Folder(_) => old,
            Self::PdfPages(..) => match answer {
                Some(Answer::PdfPages(Some(_))) => false,
                _ => old,
            },
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::File(path) | Self::Folder(path) | Self::PdfPages(path, ..) => path,
        }
    }
}

/// The disk a path is on, as far as its path tells: a volume of its own
/// under `/Volumes` (a stick, a drive, a share mounted there), a share under
/// `/Network` or `/net`, else the Mac's own disk. Each is looked at by a
/// thread of its own.
fn place_of(path: &Path) -> PathBuf {
    let mut parts = path.components();
    let (Some(root), Some(top)) = (parts.next(), parts.next()) else {
        return PathBuf::from("/");
    };
    let top_name = top.as_os_str();
    let under = |levels: usize, parts: std::path::Components<'_>| {
        let mut place = PathBuf::from(root.as_os_str());
        place.push(top_name);
        for part in parts.take(levels) {
            place.push(part.as_os_str());
        }
        place
    };
    if top_name == "Volumes" || top_name == "net" {
        under(1, parts)
    } else if top_name == "Network" {
        under(2, parts)
    } else {
        PathBuf::from("/")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    File(Option<Found>),
    /// Shared: handed out under the lock without copying the names.
    Folder(Option<Arc<Vec<String>>>),
    PdfPages(Option<u32>),
}

/// Where the answers come from: the disk, or a stand-in in tests.
pub trait Disk: Send + 'static {
    /// The regular file at `path`; none when there is none.
    fn file(&self, path: &Path) -> Option<Found>;
    /// The names in the folder `dir`, sorted; none when it does not open.
    fn folder(&self, dir: &Path) -> Option<Vec<String>>;
    /// How many pages the PDF at `path` has; none when it does not open.
    fn pdf_pages(&self, path: &Path) -> Option<u32>;
}

/// The disk itself.
pub struct RealDisk;

impl Disk for RealDisk {
    fn file(&self, path: &Path) -> Option<Found> {
        let meta = std::fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
        Some(Found {
            len: meta.len(),
            modified: meta.modified().ok(),
            real: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        })
    }

    fn folder(&self, dir: &Path) -> Option<Vec<String>> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Some(names)
    }

    fn pdf_pages(&self, path: &Path) -> Option<u32> {
        crate::presentation::pdf::PdfPages::open(path)
            .ok()
            .map(|doc| doc.pages())
    }
}

/// An answer as kept: what was found and when, whether a thread has the
/// question now, and when it was last asked for.
struct Entry {
    answer: Option<Answer>,
    found: Option<Instant>,
    asked: bool,
    used: Instant,
}

/// What the threads share with the askers: the answers, and when they were
/// last swept of those no one asks for.
struct Known {
    answers: HashMap<Question, Entry>,
    swept: Instant,
}

impl Known {
    /// Let go of the answers no one asked for in a while — in a minute when
    /// there are too many — giving them back to be dropped elsewhere.
    fn sweep(&mut self, now: Instant) -> Vec<Entry> {
        let crowded = self.answers.len() > KEPT_AT_MOST;
        let every = if crowded { Duration::from_secs(1) } else { SWEEP };
        if now.duration_since(self.swept) < every {
            return Vec::new();
        }
        self.swept = now;
        let unused = if crowded { Duration::from_secs(60) } else { FORGOTTEN };
        let gone: Vec<Question> = self
            .answers
            .iter()
            .filter(|(_, entry)| !entry.asked && now.duration_since(entry.used) >= unused)
            .map(|(question, _)| question.clone())
            .collect();
        gone.iter()
            .filter_map(|question| self.answers.remove(question))
            .collect()
    }
}

/// Each disk's thread, by [`place_of`]: where its questions go.
type Places = Mutex<HashMap<PathBuf, mpsc::SyncSender<Question>>>;

/// The answers, and the threads finding them: one a disk.
pub struct Facts {
    known: Arc<Mutex<Known>>,
    disk: Arc<dyn Disk + Sync>,
    places: Arc<Places>,
    /// How long a disk's thread waits for a question before it ends.
    idle: Duration,
}

impl Facts {
    /// Answers from `disk`, found by threads of their own.
    pub fn with(disk: impl Disk + Sync) -> Self {
        Self::idle_for(disk, IDLE)
    }

    /// [`Facts::with`], each disk's thread ending once idle for `idle`.
    fn idle_for(disk: impl Disk + Sync, idle: Duration) -> Self {
        Self {
            idle,
            known: Arc::new(Mutex::new(Known {
                answers: HashMap::new(),
                swept: Instant::now(),
            })),
            disk: Arc::new(disk),
            places: Arc::default(),
        }
    }

    /// Put `question` to the thread of its disk, starting that thread the
    /// first time; whether it took it. Put with the places held: a thread
    /// ending idle takes itself out of them first, then hands back what
    /// came (see [`find`]).
    fn put(&self, question: &Question) -> bool {
        let place = place_of(question.path());
        let mut places = self
            .places
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !places.contains_key(&place) {
            let (ask, questions) = mpsc::sync_channel(WAITING);
            let (disk, known) = (Arc::clone(&self.disk), Arc::clone(&self.known));
            let (all, mine, idle) = (Arc::clone(&self.places), place.clone(), self.idle);
            let started = std::thread::Builder::new()
                .name("file-facts".into())
                .spawn(move || find(&*disk, &known, &questions, (&all, &mine), idle));
            if let Err(e) = started {
                // Not known then: no show thread asks the disk.
                log::warn!("Files on {} are not looked at: no thread ({e})", place.display());
                return false;
            }
            places.insert(place.clone(), ask);
        }
        places
            .get(&place)
            .is_some_and(|ask| ask.try_send(question.clone()).is_ok())
    }

    /// The last answer to `question`, none before the first; put to its
    /// disk's thread again when it is stale.
    fn answer(&self, question: &Question) -> Option<Answer> {
        let now = Instant::now();
        let mut known = self
            .known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = known.answers.entry(question.clone()).or_insert(Entry {
            answer: None,
            found: None,
            asked: false,
            used: now,
        });
        entry.used = now;
        let answer = entry.answer.clone();
        let wanted = !entry.asked && question.stale(entry.answer.as_ref(), entry.found, now);
        if wanted {
            entry.asked = true;
        }
        let gone = known.sweep(now);
        drop(known);
        drop(gone);
        if wanted && !self.put(question) {
            // Not taken (too many waiting): put again when next asked.
            if let Some(entry) = self
                .known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .answers
                .get_mut(question)
            {
                entry.asked = false;
            }
        }
        answer
    }

    /// The regular file at `path` as last found: `Some(None)` when there
    /// was none, `None` when not yet looked at.
    pub fn file(&self, path: &Path) -> Option<Option<Found>> {
        match self.answer(&Question::File(path.to_path_buf()))? {
            Answer::File(found) => Some(found),
            _ => None,
        }
    }

    /// The names in the folder `dir` as last found, sorted: `Some(None)`
    /// when it did not open, `None` when not yet looked at.
    pub fn folder(&self, dir: &Path) -> Option<Option<Arc<Vec<String>>>> {
        match self.answer(&Question::Folder(dir.to_path_buf()))? {
            Answer::Folder(names) => Some(names),
            _ => None,
        }
    }

    /// How many pages the PDF `found` at `path` has, counted once for its
    /// size and change time: `Some(None)` when it did not open, `None` when
    /// not yet counted.
    pub fn pdf_pages(&self, path: &Path, found: &Found) -> Option<Option<u32>> {
        let question = Question::PdfPages(path.to_path_buf(), found.len, found.modified);
        match self.answer(&question)? {
            Answer::PdfPages(pages) => Some(pages),
            _ => None,
        }
    }

    /// Wait, at most `timeout`, until every question put so far has its
    /// answer: for tests. False if one is still out.
    pub fn settle(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            let out = self
                .known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .answers
                .values()
                .any(|entry| entry.asked);
            if !out {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// A disk's thread: answer each question from `disk` as it comes, and now
/// and then let go of the answers no one asks for — every [`SWEEP`] even
/// when no question comes. Idle for `idle`, it takes itself out of the
/// places (as `mine`) and ends, handing back any question that came in the
/// meantime to be put again.
fn find(
    disk: &dyn Disk,
    known: &Mutex<Known>,
    questions: &mpsc::Receiver<Question>,
    (places, mine): (&Places, &Path),
    idle: Duration,
) {
    let mut last = Instant::now();
    loop {
        let question = match questions.recv_timeout(SWEEP.min(idle)) {
            Ok(question) => Some(question),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let now = Instant::now();
        if question.is_some() {
            last = now;
        } else if now.duration_since(last) >= idle {
            places
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(mine);
            // Out of the places: nothing more comes. What came before is
            // put again when next asked.
            let mut known = known
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for question in questions.try_iter() {
                if let Some(entry) = known.answers.get_mut(&question) {
                    entry.asked = false;
                }
            }
            return;
        }
        // Found with nothing held: only this thread waits on the disk.
        let answer = question.as_ref().map(|question| match question {
            Question::File(path) => Answer::File(disk.file(path)),
            Question::Folder(dir) => Answer::Folder(disk.folder(dir).map(Arc::new)),
            Question::PdfPages(path, ..) => Answer::PdfPages(disk.pdf_pages(path)),
        });
        let now = Instant::now();
        let mut known = known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut replaced = None;
        if let (Some(question), Some(answer)) = (question, answer) {
            if let Some(entry) = known.answers.get_mut(&question) {
                replaced = entry.answer.replace(answer);
                entry.found = Some(now);
                entry.asked = false;
            }
        }
        let gone = known.sweep(now);
        drop(known);
        drop((replaced, gone));
    }
}

/// The show's answers, from the disk.
pub fn facts() -> &'static Facts {
    static FACTS: OnceLock<Facts> = OnceLock::new();
    FACTS.get_or_init(|| Facts::with(RealDisk))
}

/// [`Facts::file`] of the show's answers.
pub fn file(path: &Path) -> Option<Option<Found>> {
    facts().file(path)
}

/// [`Facts::folder`] of the show's answers.
pub fn folder(dir: &Path) -> Option<Option<Arc<Vec<String>>>> {
    facts().folder(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A disk whose answers the test sets, that can be held up, and counts
    /// how often it was asked.
    #[derive(Clone, Default)]
    struct FakeDisk {
        files: Arc<Mutex<HashMap<PathBuf, Found>>>,
        held: Arc<(Mutex<bool>, std::sync::Condvar)>,
        asked: Arc<AtomicU32>,
        pdfs_counted: Arc<AtomicU32>,
        pdf_refused: Arc<std::sync::atomic::AtomicBool>,
        /// Held, a PDF under /Volumes/dead never answers until let go.
        dead: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl FakeDisk {
        fn hold(&self, held: bool) {
            *self.held.0.lock().unwrap() = held;
            self.held.1.notify_all();
        }
        fn wait_while_held(&self) {
            let mut held = self.held.0.lock().unwrap();
            while *held {
                held = self.held.1.wait(held).unwrap();
            }
        }
        fn put(&self, path: &str, len: u64) {
            self.files.lock().unwrap().insert(
                PathBuf::from(path),
                Found {
                    len,
                    modified: None,
                    real: PathBuf::from(path),
                },
            );
        }
    }

    impl Disk for FakeDisk {
        fn file(&self, path: &Path) -> Option<Found> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.wait_while_held();
            self.files.lock().unwrap().get(path).cloned()
        }
        fn folder(&self, _dir: &Path) -> Option<Vec<String>> {
            self.wait_while_held();
            let mut names: Vec<String> = self
                .files
                .lock()
                .unwrap()
                .keys()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Some(names)
        }
        fn pdf_pages(&self, path: &Path) -> Option<u32> {
            self.pdfs_counted.fetch_add(1, Ordering::SeqCst);
            if path.starts_with("/Volumes/dead") {
                let mut held = self.dead.0.lock().unwrap();
                while *held {
                    held = self.dead.1.wait(held).unwrap();
                }
            }
            (!self.pdf_refused.load(Ordering::SeqCst)).then_some(12)
        }
    }

    /// A disk that keeps the thread waiting never keeps the asker waiting:
    /// the answer is "not yet", then the last one found.
    #[test]
    fn a_held_up_disk_never_holds_up_the_asker() {
        let disk = FakeDisk::default();
        disk.put("/talks/a.pptx", 10);
        disk.hold(true);
        let facts = Facts::with(disk.clone());
        let asking = std::thread::spawn(move || {
            let first = facts.file(Path::new("/talks/a.pptx"));
            let again = facts.file(Path::new("/talks/a.pptx"));
            (first, again, facts)
        });
        let until = Instant::now() + Duration::from_secs(2);
        while !asking.is_finished() {
            assert!(Instant::now() < until, "the asker waited on the held-up disk");
            std::thread::sleep(Duration::from_millis(5));
        }
        let (first, again, facts) = asking.join().unwrap();
        assert_eq!((first, again), (None, None), "not known yet");
        disk.hold(false);
        assert!(facts.settle(Duration::from_secs(5)));
        let found = facts.file(Path::new("/talks/a.pptx")).unwrap().unwrap();
        assert_eq!(found.len, 10);
        assert_eq!(disk.asked.load(Ordering::SeqCst), 1, "one question, asked twice");
    }

    /// A file gone is said to be gone once looked at again — within a
    /// second or so — and one there is said to be there.
    #[test]
    fn a_file_that_goes_is_found_gone_when_looked_at_again() {
        let disk = FakeDisk::default();
        disk.put("/talks/a.pptx", 10);
        let facts = Facts::with(disk.clone());
        assert_eq!(facts.file(Path::new("/talks/a.pptx")), None);
        assert!(facts.settle(Duration::from_secs(5)));
        assert!(facts.file(Path::new("/talks/a.pptx")).unwrap().is_some());
        assert_eq!(facts.file(Path::new("/talks/b.pptx")), None);
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.file(Path::new("/talks/b.pptx")), Some(None), "no such file");

        disk.files.lock().unwrap().clear();
        // Still the last answer until it is a second old.
        assert!(facts.file(Path::new("/talks/a.pptx")).unwrap().is_some());
        std::thread::sleep(FRESH);
        let _ = facts.file(Path::new("/talks/a.pptx"));
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.file(Path::new("/talks/a.pptx")), Some(None), "gone now");
    }

    /// A PDF's pages are counted once for its size and change time, however
    /// often asked; counted again once it changes.
    #[test]
    fn a_pdf_is_counted_once_until_it_changes() {
        let disk = FakeDisk::default();
        let facts = Facts::with(disk.clone());
        let mut found = Found {
            len: 100,
            modified: None,
            real: PathBuf::from("/p/a.pdf"),
        };
        assert_eq!(facts.pdf_pages(Path::new("/p/a.pdf"), &found), None);
        assert!(facts.settle(Duration::from_secs(5)));
        for _ in 0..5 {
            assert_eq!(facts.pdf_pages(Path::new("/p/a.pdf"), &found), Some(Some(12)));
        }
        std::thread::sleep(FRESH);
        assert_eq!(facts.pdf_pages(Path::new("/p/a.pdf"), &found), Some(Some(12)));
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(disk.pdfs_counted.load(Ordering::SeqCst), 1);
        found.len = 200;
        let _ = facts.pdf_pages(Path::new("/p/a.pdf"), &found);
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(disk.pdfs_counted.load(Ordering::SeqCst), 2, "changed: counted again");
    }

    /// A PDF that would not open is tried again — its file the same, but
    /// readable now — while one counted stands.
    #[test]
    fn a_pdf_that_would_not_open_is_tried_again() {
        let disk = FakeDisk::default();
        disk.pdf_refused.store(true, Ordering::SeqCst);
        let facts = Facts::with(disk.clone());
        let found = Found {
            len: 100,
            modified: None,
            real: PathBuf::from("/p/a.pdf"),
        };
        let _ = facts.pdf_pages(Path::new("/p/a.pdf"), &found);
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.pdf_pages(Path::new("/p/a.pdf"), &found), Some(None));
        disk.pdf_refused.store(false, Ordering::SeqCst);
        std::thread::sleep(FRESH);
        let _ = facts.pdf_pages(Path::new("/p/a.pdf"), &found);
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.pdf_pages(Path::new("/p/a.pdf"), &found), Some(Some(12)));
    }

    /// A disk that never answers — a share gone — keeps only its own thread
    /// waiting: files on the Mac's own disk are still looked at.
    #[test]
    fn a_dead_disk_holds_up_only_its_own_files() {
        let disk = FakeDisk::default();
        disk.put("/Users/me/talks/a.pptx", 10);
        *disk.dead.0.lock().unwrap() = true;
        let facts = Facts::with(disk.clone());
        let found = Found {
            len: 1,
            modified: None,
            real: PathBuf::from("/Volumes/dead/a.pdf"),
        };
        let _ = facts.pdf_pages(Path::new("/Volumes/dead/a.pdf"), &found);
        let until = Instant::now() + Duration::from_secs(5);
        while disk.pdfs_counted.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < until, "the dead disk's thread took its question");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Held up now on the dead disk; the Mac's own disk answers.
        let _ = facts.file(Path::new("/Users/me/talks/a.pptx"));
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if facts.file(Path::new("/Users/me/talks/a.pptx")).flatten().is_some() {
                break;
            }
            assert!(Instant::now() < until, "the Mac's own disk waited on the dead one");
            std::thread::sleep(Duration::from_millis(5));
        }
        *disk.dead.0.lock().unwrap() = false;
        disk.dead.1.notify_all();
    }

    /// A disk's thread idle long enough ends; a question after it starts
    /// another, and is answered.
    #[test]
    fn an_idle_disks_thread_ends_and_another_starts_when_asked() {
        let disk = FakeDisk::default();
        disk.put("/Volumes/STICK/a.pptx", 1);
        disk.put("/Volumes/STICK/b.pptx", 2);
        let facts = Facts::idle_for(disk, Duration::from_millis(100));
        let _ = facts.file(Path::new("/Volumes/STICK/a.pptx"));
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.places.lock().unwrap().len(), 1);
        let until = Instant::now() + Duration::from_secs(5);
        while !facts.places.lock().unwrap().is_empty() {
            assert!(Instant::now() < until, "the idle thread ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = facts.file(Path::new("/Volumes/STICK/b.pptx"));
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(facts.file(Path::new("/Volumes/STICK/b.pptx")).unwrap().unwrap().len, 2);
    }

    #[test]
    fn each_volume_is_a_place_of_its_own() {
        for (path, place) in [
            ("/Users/me/talks/a.pptx", "/"),
            ("/Volumes/STICK/talks/a.pptx", "/Volumes/STICK"),
            ("/Volumes/STICK", "/Volumes/STICK"),
            ("/Volumes/SSD-APFS/x/y.pdf", "/Volumes/SSD-APFS"),
            ("/Network/Servers/nas/share/a.pdf", "/Network/Servers/nas"),
            ("/net/nas/share/a.pdf", "/net/nas"),
            ("relative/a.pdf", "/"),
            ("/", "/"),
        ] {
            assert_eq!(place_of(Path::new(path)), PathBuf::from(place), "{path}");
        }
    }

    #[test]
    fn a_folders_names_come_sorted() {
        let disk = FakeDisk::default();
        disk.put("/luts/b.cube", 1);
        disk.put("/luts/a.cube", 1);
        let facts = Facts::with(disk);
        let _ = facts.folder(Path::new("/luts"));
        assert!(facts.settle(Duration::from_secs(5)));
        assert_eq!(
            facts.folder(Path::new("/luts")),
            Some(Some(Arc::new(vec!["a.cube".to_string(), "b.cube".to_string()])))
        );
    }
}
