//! Where Nida's log goes. Opened from the Finder, an app has no terminal to
//! print to. The app `scripts/make-local-app.sh` makes names a file in
//! `VARDA_LOG_FILE`; the one given away in a DMG names none, and everything
//! it printed went nowhere — its warnings, its errors, a log for the user to
//! send. So Nida's program inside an app bundle, opened where what it prints
//! goes nowhere, logs to `~/Library/Logs/Nida/nida.log` unless told
//! otherwise; run from a terminal, it prints there as any program does.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Past this size a log moves aside as `<file>.old` and starts afresh — at
/// launch, and as it grows (see `main.rs`): a log and one before it, no more.
pub const LOG_CAP: u64 = 20 * 1024 * 1024;

/// The file to log to: the one `given` (`VARDA_LOG_FILE`) names; else, when
/// `exe` (Nida's program) is inside an app bundle on a Mac and what it prints
/// `goes_nowhere` (stdout and stderr are `/dev/null`, as from the Finder),
/// `~/Library/Logs/Nida/nida.log` under `home`; else none — a program run
/// from a terminal prints there.
pub fn log_file(
    given: Option<&OsStr>,
    exe: Option<&Path>,
    home: Option<&Path>,
    goes_nowhere: bool,
) -> Option<PathBuf> {
    if let Some(given) = given {
        return Some(PathBuf::from(given));
    }
    if !cfg!(target_os = "macos") || !goes_nowhere || !exe.is_some_and(in_app_bundle) {
        return None;
    }
    home.map(|home| home.join("Library/Logs/Nida/nida.log"))
}

/// Whether `exe` is an app bundle's program: `<name>.app/Contents/MacOS/<exe>`.
fn in_app_bundle(exe: &Path) -> bool {
    let macos = exe.parent();
    let contents = macos.and_then(Path::parent);
    let app = contents.and_then(Path::parent);
    macos.and_then(Path::file_name) == Some(OsStr::new("MacOS"))
        && contents.and_then(Path::file_name) == Some(OsStr::new("Contents"))
        && app.and_then(Path::extension) == Some(OsStr::new("app"))
}

/// Start the log at `path` afresh when it has grown past `cap`, keeping the
/// one before as `<path>.old` (in place of an older one).
///
/// # Errors
///
/// Returns the error if the log cannot be moved aside.
pub fn keep_small(path: &Path, cap: u64) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > cap) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        std::fs::rename(path, PathBuf::from(old))?;
    }
    Ok(())
}

/// How many lines may wait for the log's writer before new ones are left
/// out: ~1.5 MB at ~150 bytes a line.
pub const LOG_QUEUE: usize = 10_000;

/// What waits for the writer: a line, a mark it lets go of once every line
/// before it is written (or refused) — the queue keeps their order — or
/// word that the file was moved aside and starts afresh.
enum Item {
    Line(Vec<u8>),
    Mark(Arc<(Mutex<bool>, Condvar)>),
    Reopen,
}

/// What the writer writes the log into: stderr (a terminal), or the log
/// file ([`LogFile`]).
pub trait LogSink: Write + Send {
    /// Start afresh at the file's path — the file was moved aside or is
    /// gone. Nothing to do for stderr.
    fn reopen(&mut self) {}
}

impl LogSink for std::io::Stderr {}

/// The writer's queue, for `drain` and `report`.
static QUEUE: OnceLock<mpsc::SyncSender<Item>> = OnceLock::new();

/// The writer thread did not start: nothing goes to the log at all.
static NO_WRITER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the logger writes each line into: the line is handed to a thread of
/// its own that writes it, and logging goes straight on. Written in place,
/// a write the system held up held up whoever logged — a test Nida on the
/// mini, 2026-11-02: its main thread stuck in a log `write()` for 15 hours
/// under a memory squeeze, and the show thread, waiting for it, with it.
/// Now only the writer waits; past `LOG_QUEUE` lines waiting, new lines are
/// left out, and the next one let in says how many.
pub struct QueuedLog {
    line: Vec<u8>,
    to: mpsc::SyncSender<Item>,
    left_out: u64,
}

impl Write for QueuedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// The logger flushes after each line: it goes to the writer now.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.line.is_empty() {
            return Ok(());
        }
        let mut line = std::mem::take(&mut self.line);
        if self.left_out > 0 {
            let note = format!(
                "[log] {} lines left out here: writing the log was held up\n",
                self.left_out
            );
            line.splice(0..0, note.into_bytes());
        }
        if self.to.try_send(Item::Line(line)).is_ok() {
            self.left_out = 0;
        } else {
            self.left_out += 1;
        }
        Ok(())
    }
}

impl QueuedLog {
    /// Lines into the writer's queue `to`.
    fn into_queue(to: mpsc::SyncSender<Item>) -> Self {
        Self {
            line: Vec::new(),
            to,
            left_out: 0,
        }
    }
}

/// A `QueuedLog` whose lines a thread writes to `to`, holding at most
/// `capacity` lines waiting.
fn queue_into(to: Box<dyn LogSink>, capacity: usize) -> std::io::Result<QueuedLog> {
    let (sender, items) = mpsc::sync_channel::<Item>(capacity);
    std::thread::Builder::new()
        .name("log-writer".into())
        .spawn(move || {
            let mut to = to;
            // Lines the file refused (full, gone): said once it takes one.
            let mut failed: u64 = 0;
            for item in items {
                match item {
                    Item::Line(line) => {
                        if failed > 0 {
                            let note = format!(
                                "[log] {failed} lines could not be written here: \
                                 the file refused them\n"
                            );
                            if to.write_all(note.as_bytes()).is_ok() {
                                failed = 0;
                            }
                        }
                        if to.write_all(&line).and_then(|()| to.flush()).is_err() {
                            failed += 1;
                        }
                    }
                    Item::Mark(mark) => {
                        let (reached, wake) = &*mark;
                        if let Ok(mut reached) = reached.lock() {
                            *reached = true;
                        }
                        wake.notify_all();
                    }
                    Item::Reopen => to.reopen(),
                }
            }
        })?;
    Ok(QueuedLog::into_queue(sender))
}

/// Where the logger writes: through the writer thread to the log file when
/// [`log_into_file`] set one up, else to stderr. Should that thread not
/// start, nothing is logged — written in place, a held-up write would hold
/// up the show again.
pub fn queued_stderr() -> Box<dyn Write + Send> {
    if let Some(to) = QUEUE.get() {
        return Box::new(QueuedLog::into_queue(to.clone()));
    }
    if let Ok(log) = queue_into(Box::new(std::io::stderr()), LOG_QUEUE) {
        let _ = QUEUE.set(log.to.clone());
        Box::new(log)
    } else {
        NO_WRITER.store(true, std::sync::atomic::Ordering::Release);
        Box::new(std::io::sink())
    }
}

/// Put `item` into `to`, waiting for room until `until` should it be full.
fn send_by(to: &mpsc::SyncSender<Item>, mut item: Item, until: Instant) -> bool {
    loop {
        match to.try_send(item) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(back)) if Instant::now() < until => {
                item = back;
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return false,
        }
    }
}

/// [`drain`] for the queue `to`: a mark after the lines in it, and a wait
/// for the writer to reach it.
fn drain_into(to: &mpsc::SyncSender<Item>, timeout: Duration) -> bool {
    let until = Instant::now() + timeout;
    let mark = Arc::new((Mutex::new(false), Condvar::new()));
    if !send_by(to, Item::Mark(Arc::clone(&mark)), until) {
        return false;
    }
    let (reached, wake) = &*mark;
    let Ok(mut reached) = reached.lock() else {
        return false;
    };
    while !*reached {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match wake.wait_timeout(reached, left) {
            Ok((now, _)) => reached = now,
            Err(_) => return false,
        }
    }
    true
}

/// Wait, at most `timeout`, for the lines logged so far to be written — as
/// Nida quits, and after a crash's report — so the last of them reach the
/// log. False if they did not in time.
pub fn drain(timeout: Duration) -> bool {
    let Some(to) = QUEUE.get() else {
        return true;
    };
    let until = Instant::now() + timeout;
    // What libraries printed first into the queue (see `log_into_file`).
    #[cfg(unix)]
    {
        let from = PIPE_READ.load(std::sync::atomic::Ordering::Acquire);
        if from >= 0 && !pump_caught_up(from, &PUMP_GATE, until) {
            return false;
        }
    }
    drain_into(to, until.saturating_duration_since(Instant::now()))
}

/// Put `text` (a whole line, or lines, ending in a newline) into the log
/// whatever the log's filters say — a crash's report — waiting for room for
/// at most `timeout`; straight to stderr when the log was never set up (a
/// program run without one, tests), not at all when its writer would not
/// start. False if it could not.
pub fn report(text: &str, timeout: Duration) -> bool {
    let Some(to) = QUEUE.get() else {
        if NO_WRITER.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        eprint!("{text}");
        return true;
    };
    report_into(to, text, timeout)
}

/// [`report`] into the queue `to`.
fn report_into(to: &mpsc::SyncSender<Item>, text: &str, timeout: Duration) -> bool {
    send_by(to, Item::Line(text.as_bytes().to_vec()), Instant::now() + timeout)
}

/// The log file, as the writer holds it: one descriptor that stays the same
/// when the file starts afresh — the new file is put in its place — so an
/// abort's last words always have an open file to go to (see
/// [`copy_left_on_abort`]).
#[cfg(unix)]
struct LogFile {
    path: PathBuf,
    file: std::fs::File,
}

#[cfg(unix)]
impl LogFile {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }
}

#[cfg(unix)]
impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(unix)]
impl LogSink for LogFile {
    fn reopen(&mut self) {
        use std::os::fd::AsRawFd;
        let Ok(fresh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        let ours = self.file.as_raw_fd();
        // SAFETY: both descriptors are open; dup2 puts the fresh file in
        // place of ours in one step, so ours never stands closed. It also
        // clears close-on-exec, put back: programs Nida starts get no copy.
        unsafe {
            if libc::dup2(fresh.as_raw_fd(), ours) != -1 {
                libc::fcntl(ours, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
    }
}

/// A copy of the pipe's read end kept open to the end, and the log file's
/// descriptor: what an abort reads its last words from and writes them to,
/// and what [`drain`] looks at to know the pump has caught up. -1 for none.
#[cfg(unix)]
static PIPE_READ: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
#[cfg(unix)]
static LOG_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The last [`TAIL`] bytes of what came down the pipe, kept for an abort to
/// write: the pump may already have taken a library's last words off the
/// pipe, still waiting in the queue when the process ends.
#[cfg(unix)]
struct Tail {
    bytes: [std::sync::atomic::AtomicU8; TAIL],
    /// How many bytes ever went in.
    end: std::sync::atomic::AtomicUsize,
}

#[cfg(unix)]
const TAIL: usize = 16 * 1024;

#[cfg(unix)]
impl Tail {
    const fn new() -> Self {
        Self {
            bytes: [const { std::sync::atomic::AtomicU8::new(0) }; TAIL],
            end: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Keep `bytes` as the last of it. One thread keeps: the pump.
    fn keep(&self, bytes: &[u8]) {
        use std::sync::atomic::Ordering;
        let skipped = bytes.len().saturating_sub(TAIL);
        let mut end = self.end.load(Ordering::Relaxed) + skipped;
        for &byte in &bytes[skipped..] {
            self.bytes[end % TAIL].store(byte, Ordering::Relaxed);
            end += 1;
        }
        self.end.store(end, Ordering::Release);
    }

    /// How many bytes ever went in.
    fn end(&self) -> usize {
        self.end.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Write it to the file `to`, after a line saying what it is; nothing
    /// when nothing was kept. False if the file would not take it. Only
    /// calls an abort may make (write).
    fn write_to(&self, to: libc::c_int) -> bool {
        let end = self.end();
        if end == 0 {
            return true;
        }
        let note = b"\n[log] The last of what was printed before Nida aborted \
                     (some of it may be above already):\n";
        put_all(to, note) && self.write_range(to, end.saturating_sub(TAIL), end)
    }

    /// Write the bytes that went in from the `from`th to the `end`th — those
    /// still kept — to the file `to`. False if it would not take them. Only
    /// calls an abort may make (write).
    fn write_range(&self, to: libc::c_int, from: usize, end: usize) -> bool {
        use std::sync::atomic::Ordering;
        let mut buf = [0_u8; 4096];
        let mut at = from.max(end.saturating_sub(TAIL));
        while at < end {
            let n = (end - at).min(buf.len());
            for (i, byte) in buf[..n].iter_mut().enumerate() {
                *byte = self.bytes[(at + i) % TAIL].load(Ordering::Relaxed);
            }
            if !put_all(to, &buf[..n]) {
                return false;
            }
            at += n;
        }
        true
    }
}

/// The tail the pump keeps.
#[cfg(unix)]
static TAIL_KEPT: Tail = Tail::new();

/// The pump's word with an abort: it counts up before it reads and again
/// once what it read is in the tail — odd while reading — and reads no more
/// once `aborting` is set. An abort sets it and waits for an even count:
/// then everything the pump took is in the tail, and whatever is left is
/// still in the pipe for the abort to copy.
#[cfg(unix)]
struct PumpGate {
    count: std::sync::atomic::AtomicU64,
    aborting: std::sync::atomic::AtomicBool,
    /// Whether the pump has something in hand: a piece read and not yet
    /// taken to the queue, or a line begun and not ended.
    busy: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl PumpGate {
    const fn new() -> Self {
        Self {
            count: std::sync::atomic::AtomicU64::new(0),
            aborting: std::sync::atomic::AtomicBool::new(false),
            busy: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Stop the pump reading, and wait — a tenth of a second at most — for
    /// a read under way to be in the tail: whether it is. Only calls an
    /// abort may make (nanosleep).
    fn hold(&self) -> bool {
        self.aborting.store(true, std::sync::atomic::Ordering::SeqCst);
        self.settled()
    }

    /// Wait, a tenth of a second at most, for no read to be under way:
    /// whether none is. Only calls an abort may make (nanosleep).
    fn settled(&self) -> bool {
        for _ in 0..100 {
            if self
                .count
                .load(std::sync::atomic::Ordering::SeqCst)
                .is_multiple_of(2)
            {
                return true;
            }
            let a_millisecond = libc::timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000,
            };
            // SAFETY: a plain sleep.
            unsafe { libc::nanosleep(&raw const a_millisecond, std::ptr::null_mut()) };
        }
        false
    }
}

/// The gate of the pump [`log_into_file`] starts.
#[cfg(unix)]
static PUMP_GATE: PumpGate = PumpGate::new();

/// Write all of `bytes` to `to`; false if it would not take them. Only
/// calls an abort may make (write).
#[cfg(unix)]
fn put_all(to: libc::c_int, bytes: &[u8]) -> bool {
    let mut done = 0;
    while done < bytes.len() {
        // SAFETY: a write from `bytes`, which holds what is asked.
        let put = unsafe { libc::write(to, bytes[done..].as_ptr().cast(), bytes.len() - done) };
        match usize::try_from(put) {
            Ok(put) if put > 0 => done += put,
            _ => return false,
        }
    }
    true
}

/// Log to the file at `path` — Nida's own lines and whatever its libraries
/// print (`FFmpeg`'s, the system's) — with no thread but the writer waiting
/// on the file.
///
/// stdout and stderr go into a pipe; a thread of its own takes what comes
/// down it a line at a time into the writer's queue, leaving lines out
/// rather than waiting when the queue is full. Pointed at the file
/// itself, a library's message waited on the file in the thread that
/// printed it — a show's, opening a clip — as Nida's own lines did before
/// they were queued (see [`QueuedLog`]). Should Nida abort, the last of
/// what came down the pipe goes to the file first: a library's last words
/// before an abort are often why.
///
/// # Errors
///
/// When the file, the pipe or either thread cannot be had. stdout and
/// stderr are left as they were then: pointed straight at the file
/// instead, a library's message would wait on it again, and do so just
/// when the system is short of what Nida asked for.
#[cfg(unix)]
pub fn log_into_file(path: &Path) -> std::io::Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let file = LogFile::open(path)?;
    let log_fd = file.file.as_raw_fd();
    let log = queue_into(Box::new(file), LOG_QUEUE)?;
    let mut ends = [0; 2];
    // SAFETY: `ends` holds the two descriptors pipe() fills.
    if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both just made by pipe(), owned here alone.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    // A copy of the read end for an abort and for `drain`, open to the end:
    // never closed under them and its number given to another file.
    let kept = read.try_clone()?;
    for end in [&read, &write, &kept] {
        // Not handed to programs Nida starts (FFmpeg): stdout and stderr,
        // below, are.
        // SAFETY: an open descriptor.
        unsafe { libc::fcntl(end.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    pump_into(read, log.to.clone(), &PUMP_GATE, &TAIL_KEPT)?;
    // SAFETY: `write` is open; dup2 onto stdout and stderr replaces them
    // for the rest of the process.
    let pointed = unsafe {
        libc::dup2(write.as_raw_fd(), libc::STDOUT_FILENO) != -1
            && libc::dup2(write.as_raw_fd(), libc::STDERR_FILENO) != -1
    };
    if !pointed {
        return Err(std::io::Error::last_os_error());
    }
    PIPE_READ.store(
        std::os::fd::IntoRawFd::into_raw_fd(kept),
        std::sync::atomic::Ordering::Release,
    );
    LOG_FD.store(log_fd, std::sync::atomic::Ordering::Release);
    copy_left_on_abort();
    let _ = QUEUE.set(log.to);
    Ok(())
}

/// Have the log file set up by [`log_into_file`] start afresh at its path —
/// after it was moved aside, or went missing. False if there is none, or
/// its writer would not take the word in a second.
pub fn reopen() -> bool {
    QUEUE
        .get()
        .is_some_and(|to| send_by(to, Item::Reopen, Instant::now() + Duration::from_secs(1)))
}

/// How long the pump keeps a line begun and not ended before it takes it
/// as it is.
#[cfg(unix)]
const PART_WAITS_MS: libc::c_int = 100;

/// A thread taking what comes down the pipe `from` into the queue `to`, a
/// line at a time, never waiting on the queue: a full queue leaves lines
/// out, and the next line let in says how many. It runs for as long as
/// anything may print: gone, the pipe would fill and printing wait again.
#[cfg(unix)]
fn pump_into(
    from: std::os::fd::OwnedFd,
    to: mpsc::SyncSender<Item>,
    gate: &'static PumpGate,
    tail: &'static Tail,
) -> std::io::Result<()> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::Ordering;
    // A line longer than this goes on in parts.
    const LONGEST: usize = 16 * 1024;
    std::thread::Builder::new()
        .name("log-pump".into())
        .spawn(move || {
            let fd = from.as_raw_fd();
            let mut from = std::fs::File::from(from);
            let mut log = QueuedLog::into_queue(to);
            let mut buf = [0_u8; 8192];
            loop {
                let mut ready = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd, for the pipe this thread owns.
                let polled = unsafe { libc::poll(&raw mut ready, 1, PART_WAITS_MS) };
                if polled < 0 {
                    // Interrupted: looked at again.
                    continue;
                }
                if polled == 0 {
                    // Nothing more came: a line begun goes as it is.
                    let _ = log.flush();
                    gate.busy.store(false, Ordering::Release);
                    continue;
                }
                gate.busy.store(true, Ordering::Release);
                // Odd: reading (see `PumpGate`).
                gate.count.fetch_add(1, Ordering::SeqCst);
                if gate.aborting.load(Ordering::SeqCst) {
                    // Nida is aborting: what is left stays in the pipe for
                    // the abort to copy.
                    gate.count.fetch_add(1, Ordering::SeqCst);
                    loop {
                        std::thread::park();
                    }
                }
                let read = from.read(&mut buf);
                if let Ok(n) = read {
                    tail.keep(&buf[..n]);
                }
                gate.count.fetch_add(1, Ordering::SeqCst);
                let n = match read {
                    // Every write end closed: nothing can print any more.
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => {
                        // Interrupted, or the read end made nonblocking by
                        // an abort: looked at again shortly.
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                };
                for part in buf[..n].split_inclusive(|&b| b == b'\n') {
                    log.line.extend_from_slice(part);
                    if part.ends_with(b"\n") || log.line.len() >= LONGEST {
                        let _ = log.flush();
                    }
                }
                if log.line.is_empty() {
                    gate.busy.store(false, Ordering::Release);
                }
            }
            let _ = log.flush();
            gate.busy.store(false, Ordering::Release);
        })?;
    Ok(())
}

/// Wait, until `until`, for the pump to take everything printed so far
/// into the queue: the pipe `from` empty and nothing in its hands. False
/// if it did not in time.
#[cfg(unix)]
fn pump_caught_up(from: libc::c_int, gate: &PumpGate, until: Instant) -> bool {
    loop {
        let mut waiting: libc::c_int = 0;
        // SAFETY: FIONREAD writes how many bytes wait into `waiting`.
        let asked = unsafe { libc::ioctl(from, libc::FIONREAD, &raw mut waiting) };
        if asked == 0 && waiting == 0 && !gate.busy.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// At most this much more is copied from the pipe as Nida aborts: what
/// keeps printing then cannot keep the abort from ending.
#[cfg(unix)]
const COPIED_AT_MOST: usize = 64 * 1024;

/// What an abort leaves in the log: the last of what came down the pipe,
/// then what is still in it.
#[cfg(unix)]
fn copy_left(from: libc::c_int, to: libc::c_int) {
    if TAIL_KEPT.write_to(to) {
        copy_pipe(from, to);
    }
}

/// Copy what is still in the pipe `from` to the file `to` — at most
/// [`COPIED_AT_MOST`] — returning once it is empty. Only calls an abort may
/// make (fcntl, read, write).
#[cfg(unix)]
fn copy_pipe(from: libc::c_int, to: libc::c_int) {
    // SAFETY: plain fcntl calls on an open descriptor.
    unsafe {
        let flags = libc::fcntl(from, libc::F_GETFL);
        if flags == -1 || libc::fcntl(from, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            return;
        }
    }
    let mut buf = [0_u8; 4096];
    let mut copied = 0;
    while copied < COPIED_AT_MOST {
        // SAFETY: a read into `buf`, which holds what is asked.
        let got = unsafe { libc::read(from, buf.as_mut_ptr().cast(), buf.len()) };
        let Some(got) = usize::try_from(got).ok().filter(|&n| n > 0) else {
            return;
        };
        if !put_all(to, &buf[..got]) {
            return;
        }
        copied += got;
    }
}

/// Should Nida abort, the last of what was printed goes to the log first;
/// then the abort goes on as it would have — crash report and all. A file
/// that keeps the copy waiting cannot keep the process from ending: two
/// seconds on, an alarm ends it.
#[cfg(unix)]
fn copy_left_on_abort() {
    extern "C" fn on_abort(_: libc::c_int) {
        // SAFETY: only calls an abort may make: sigaction, alarm, raise,
        // and in `copy_left` fcntl, read and write.
        unsafe {
            let mut alarm: libc::sigaction = std::mem::zeroed();
            alarm.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&raw mut alarm.sa_mask);
            libc::sigaction(libc::SIGALRM, &raw const alarm, std::ptr::null_mut());
            libc::alarm(2);
        }
        let from = PIPE_READ.load(std::sync::atomic::Ordering::Acquire);
        let to = LOG_FD.load(std::sync::atomic::Ordering::Acquire);
        if from >= 0 && to >= 0 {
            let settled = PUMP_GATE.hold();
            let end = TAIL_KEPT.end();
            copy_left(from, to);
            // A read under way all the while — the pump held up a tenth of a
            // second in it — is written once it is in the tail, after the
            // rest: out of order, but there.
            if !settled && PUMP_GATE.settled() {
                TAIL_KEPT.write_range(to, end, TAIL_KEPT.end());
            }
        }
        // SA_RESETHAND has put the default back: raised again, it ends the
        // process as the abort would have, once this returns.
        // SAFETY: as above.
        unsafe { libc::raise(libc::SIGABRT) };
    }
    // SAFETY: a zeroed sigaction filled in; the handler only makes calls an
    // abort may make.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_abort as extern "C" fn(libc::c_int) as usize;
        action.sa_flags = libc::SA_RESETHAND;
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGABRT, &raw const action, std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    const APP: &str = "/Applications/Nida.app/Contents/MacOS/varda";

    /// A writer that keeps what it is given, and can be held up: each write
    /// waits until let go.
    #[derive(Clone)]
    struct HeldUp {
        kept: Arc<std::sync::Mutex<Vec<u8>>>,
        held: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        entered: mpsc::Sender<()>,
    }

    impl Write for HeldUp {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.entered.send(());
            let (held, cv) = &*self.held;
            let mut held = held.lock().unwrap();
            while *held {
                held = cv.wait(held).unwrap();
            }
            self.kept.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl LogSink for HeldUp {}

    fn held_up() -> (HeldUp, mpsc::Receiver<()>) {
        let (entered, entering) = mpsc::channel();
        let writer = HeldUp {
            kept: Arc::default(),
            held: Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new())),
            entered,
        };
        (writer, entering)
    }

    fn let_go(writer: &HeldUp) {
        *writer.held.0.lock().unwrap() = false;
        writer.held.1.notify_all();
    }

    fn log_line(log: &mut QueuedLog, line: &str) {
        log.write_all(line.as_bytes()).unwrap();
        log.flush().unwrap();
    }

    /// The file's write held up: logging goes on at once, the lines past
    /// the queue are left out, and the next line let in says how many.
    #[test]
    fn a_held_up_write_never_holds_up_logging() {
        let (writer, entering) = held_up();
        let mut log = queue_into(Box::new(writer.clone()), 2).unwrap();
        let to = log.to.clone();
        log_line(&mut log, "A\n");
        entering
            .recv_timeout(Duration::from_secs(5))
            .expect("the writer took A and is held up writing it");
        // On a thread of its own, so a logger that waits fails the test
        // rather than hanging it.
        let (done, finished) = mpsc::channel();
        let logging = std::thread::spawn(move || {
            for line in ["B\n", "C\n", "D\n", "E\n", "F\n"] {
                log_line(&mut log, line);
            }
            let _ = done.send(());
            log
        });
        finished
            .recv_timeout(Duration::from_secs(2))
            .expect("logging went on while the write was held up");
        let mut log = logging.join().unwrap();
        let_go(&writer);
        assert!(drain_into(&to, Duration::from_secs(5)));
        log_line(&mut log, "G\n");
        assert!(drain_into(&to, Duration::from_secs(5)));
        let kept = String::from_utf8(writer.kept.lock().unwrap().clone()).unwrap();
        assert_eq!(
            kept,
            "A\nB\nC\n[log] 3 lines left out here: writing the log was held up\nG\n"
        );
    }

    /// A writer that refuses what it is given while `refuse` is set (a full
    /// disk), and keeps it otherwise.
    struct Refusing {
        kept: Arc<std::sync::Mutex<Vec<u8>>>,
        refuse: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Write for Refusing {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.refuse.load(Ordering::SeqCst) {
                return Err(std::io::Error::from_raw_os_error(28)); // ENOSPC
            }
            self.kept.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl LogSink for Refusing {}

    /// Lines the file refused are not passed off as written: the log says
    /// how many, once the file takes one again.
    #[test]
    fn refused_lines_are_noted_once_the_file_takes_one() {
        let kept = Arc::new(std::sync::Mutex::new(Vec::new()));
        let refuse = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = Refusing {
            kept: Arc::clone(&kept),
            refuse: Arc::clone(&refuse),
        };
        let mut log = queue_into(Box::new(writer), 16).unwrap();
        let to = log.to.clone();
        log_line(&mut log, "before\n");
        assert!(drain_into(&to, Duration::from_secs(5)));
        refuse.store(true, Ordering::SeqCst);
        log_line(&mut log, "lost 1\n");
        log_line(&mut log, "lost 2\n");
        assert!(drain_into(&to, Duration::from_secs(5)), "tried, though refused");
        refuse.store(false, Ordering::SeqCst);
        log_line(&mut log, "after\n");
        assert!(drain_into(&to, Duration::from_secs(5)));
        assert_eq!(
            String::from_utf8(kept.lock().unwrap().clone()).unwrap(),
            "before\n[log] 2 lines could not be written here: the file refused them\nafter\n"
        );
    }

    /// A crash's report finds the queue full: it waits its turn (for a
    /// while) rather than being left out.
    #[test]
    fn a_report_waits_for_room_in_a_full_queue() {
        let (writer, entering) = held_up();
        let mut log = queue_into(Box::new(writer.clone()), 1).unwrap();
        log_line(&mut log, "first\n");
        entering.recv_timeout(Duration::from_secs(5)).unwrap();
        log_line(&mut log, "second\n");
        log_line(&mut log, "left out\n");
        let to = log.to.clone();
        let reporting = {
            let to = to.clone();
            std::thread::spawn(move || report_into(&to, "PANIC\n", Duration::from_secs(5)))
        };
        std::thread::sleep(Duration::from_millis(50));
        let_go(&writer);
        assert!(reporting.join().unwrap(), "the report got in");
        assert!(drain_into(&to, Duration::from_secs(5)));
        assert_eq!(
            String::from_utf8(writer.kept.lock().unwrap().clone()).unwrap(),
            "first\nsecond\nPANIC\n"
        );
    }

    /// Draining waits for the lines logged so far, in order — and gives up
    /// after its time when the writer stays held up.
    #[test]
    fn draining_waits_for_the_lines_then_gives_up() {
        let (writer, entering) = held_up();
        let mut log = queue_into(Box::new(writer.clone()), 16).unwrap();
        let to = log.to.clone();
        for line in ["one\n", "two\n", "three\n"] {
            log_line(&mut log, line);
        }
        entering.recv_timeout(Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        assert!(!drain_into(&to, Duration::from_millis(50)), "still held up");
        assert!(started.elapsed() < Duration::from_secs(2));
        let_go(&writer);
        assert!(drain_into(&to, Duration::from_secs(5)));
        assert_eq!(
            String::from_utf8(writer.kept.lock().unwrap().clone()).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    /// A pipe, as the two files at its ends.
    #[cfg(unix)]
    fn pipe() -> (std::os::fd::OwnedFd, std::fs::File) {
        use std::os::fd::FromRawFd;
        let mut ends = [0; 2];
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(ends[0]),
                std::fs::File::from_raw_fd(ends[1]),
            )
        }
    }

    /// The log file's write held up: a library printing far more than the
    /// pipe holds goes on at once; the lines past the queue are left out,
    /// and the next one let in says how many.
    #[cfg(unix)]
    #[test]
    fn what_libraries_print_never_waits_on_a_held_up_log() {
        use std::os::fd::AsRawFd;
        let (writer, entering) = held_up();
        let log = queue_into(Box::new(writer.clone()), 4).unwrap();
        let to = log.to.clone();
        let (read, mut write) = pipe();
        let kept = read.try_clone().unwrap();
        let gate: &'static PumpGate = Box::leak(Box::new(PumpGate::new()));
        pump_into(read, to.clone(), gate, Box::leak(Box::new(Tail::new()))).unwrap();
        write.write_all(b"first\n").unwrap();
        entering
            .recv_timeout(Duration::from_secs(5))
            .expect("the writer took the first line and is held up writing it");
        // A megabyte, sixteen pipes' worth, from a thread of its own: a
        // print that waits fails the test rather than hanging it.
        let (done, finished) = mpsc::channel();
        let printing = std::thread::spawn(move || {
            for n in 0..20_000 {
                writeln!(write, "line {n:05} from a library, printed while the log is held up")
                    .unwrap();
            }
            let _ = done.send(());
            write
        });
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("printing went on while the log's write was held up");
        let mut write = printing.join().unwrap();
        let_go(&writer);
        // Room again: the pump caught up and the queue written out.
        let until = Instant::now() + Duration::from_secs(5);
        assert!(pump_caught_up(kept.as_raw_fd(), gate, until));
        assert!(drain_into(&to, Duration::from_secs(5)));
        write.write_all(b"after\n").unwrap();
        assert!(pump_caught_up(kept.as_raw_fd(), gate, until));
        assert!(drain_into(&to, Duration::from_secs(5)));
        let kept = String::from_utf8(writer.kept.lock().unwrap().clone()).unwrap();
        assert!(kept.starts_with("first\nline 00000 from a library"), "{kept:.200}");
        let note = kept
            .lines()
            .find(|line| line.starts_with("[log] "))
            .expect("the lines left out are counted");
        assert!(note.ends_with("lines left out here: writing the log was held up"), "{note}");
        assert!(
            kept.ends_with("after\n"),
            "the pump kept going: {:?}",
            &kept[kept.len().saturating_sub(200)..]
        );
    }

    /// Moved aside, the log starts afresh at its path — at the same
    /// descriptor, which an abort's last words go to, still not handed to
    /// programs Nida starts.
    #[cfg(unix)]
    #[test]
    fn a_reopened_log_starts_afresh_at_its_path() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nida.log");
        let mut file = LogFile::open(&path).unwrap();
        let fd = file.file.as_raw_fd();
        file.write_all(b"before\n").unwrap();
        std::fs::rename(&path, dir.path().join("nida.log.old")).unwrap();
        file.reopen();
        assert_eq!(file.file.as_raw_fd(), fd, "the same descriptor");
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "still closed for programs Nida starts");
        file.write_all(b"after\n").unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("nida.log.old")).unwrap(), "before\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
    }

    /// What is in the pipe as Nida aborts is copied to the log; the copy
    /// stops when the pipe is empty rather than waiting for more, and stops
    /// all the same while something keeps printing.
    #[cfg(unix)]
    #[test]
    fn whats_left_in_the_pipe_is_copied_and_the_copy_ends() {
        use std::os::fd::AsRawFd;
        let finishes = |copying: std::thread::JoinHandle<()>| {
            let until = Instant::now() + Duration::from_secs(5);
            while !copying.is_finished() {
                assert!(Instant::now() < until, "the copy did not end");
                std::thread::sleep(Duration::from_millis(5));
            }
            copying.join().unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nida.log");
        let log = LogFile::open(&path).unwrap();
        let (read, mut write) = pipe();
        write.write_all(b"Assertion failed at libavcodec/x.c:1\n").unwrap();
        let (from, to) = (read.as_raw_fd(), log.file.as_raw_fd());
        finishes(std::thread::spawn(move || copy_pipe(from, to)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Assertion failed at libavcodec/x.c:1\n"
        );

        // Printing on and on while it copies.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let printing = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let _ = write.write_all(b"more and more\n");
                }
                write
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        finishes(std::thread::spawn(move || copy_pipe(from, to)));
        stop.store(true, Ordering::SeqCst);
        // The pipe is nonblocking now: drained for the printer to end.
        let mut drain = [0_u8; 4096];
        while !printing.is_finished() {
            unsafe { libc::read(from, drain.as_mut_ptr().cast(), drain.len()) };
        }
        drop((printing.join().unwrap(), read));
        let copied = std::fs::metadata(&path).unwrap().len() as usize;
        assert!(copied <= 64 * 1024 + 4096 + 64, "bounded: {copied}");
    }

    /// Held for an abort, the pump has put all it took into the tail and
    /// takes nothing more: what comes after stays in the pipe for the abort
    /// to copy.
    #[cfg(unix)]
    #[test]
    fn a_held_pump_leaves_the_rest_in_the_pipe() {
        use std::os::fd::AsRawFd;
        let (writer, _entering) = held_up();
        let_go(&writer);
        let log = queue_into(Box::new(writer.clone()), 16).unwrap();
        let (read, mut write) = pipe();
        let kept = read.try_clone().unwrap();
        let gate: &'static PumpGate = Box::leak(Box::new(PumpGate::new()));
        let tail: &'static Tail = Box::leak(Box::new(Tail::new()));
        pump_into(read, log.to.clone(), gate, tail).unwrap();
        write.write_all(b"before\n").unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        assert!(pump_caught_up(kept.as_raw_fd(), gate, until));
        gate.hold();
        write.write_all(b"after the abort\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let mut waiting: libc::c_int = 0;
        assert_eq!(unsafe { libc::ioctl(kept.as_raw_fd(), libc::FIONREAD, &raw mut waiting) }, 0);
        assert_eq!(waiting, 16, "still in the pipe");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nida.log");
        let file = LogFile::open(&path).unwrap();
        assert!(tail.write_to(file.file.as_raw_fd()));
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("before\n"));
    }

    /// A read under way when the abort comes — the pump held up in it — is
    /// waited for only so long, and what it read written after the rest.
    #[cfg(unix)]
    #[test]
    fn a_read_under_way_is_waited_for_only_so_long() {
        use std::os::fd::AsRawFd;
        let gate = PumpGate::new();
        gate.count.store(1, Ordering::SeqCst); // a read under way
        let started = Instant::now();
        assert!(!gate.hold(), "still reading");
        assert!(started.elapsed() < Duration::from_secs(1));
        gate.count.store(2, Ordering::SeqCst);
        assert!(gate.settled());
        let tail = Tail::new();
        tail.keep(b"before the abort\n");
        let end = tail.end();
        tail.keep(b"read late\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nida.log");
        let file = LogFile::open(&path).unwrap();
        assert!(tail.write_range(file.file.as_raw_fd(), end, tail.end()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "read late\n");
    }

    /// The tail keeps the last 16 KB of what came down the pipe, and writes
    /// them after a line saying what they are.
    #[cfg(unix)]
    #[test]
    fn the_tail_keeps_the_last_of_what_was_printed() {
        use std::os::fd::AsRawFd;
        let tail = Tail::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nida.log");
        let log = LogFile::open(&path).unwrap();
        assert!(tail.write_to(log.file.as_raw_fd()));
        assert_eq!(std::fs::read(&path).unwrap(), b"", "nothing kept, nothing written");
        let printed: Vec<u8> = (0..30_000_u32).map(|n| b'a' + (n % 26) as u8).collect();
        for piece in printed.chunks(700) {
            tail.keep(piece);
        }
        tail.keep(&printed[..0]);
        assert!(tail.write_to(log.file.as_raw_fd()));
        let written = std::fs::read(&path).unwrap();
        let note = b"\n[log] The last of what was printed before Nida aborted \
                     (some of it may be above already):\n";
        assert!(written.starts_with(note));
        assert_eq!(&written[note.len()..], &printed[printed.len() - TAIL..]);
        // A piece longer than the whole tail: its own last 16 KB.
        let tail = Tail::new();
        tail.keep(&printed);
        std::fs::write(&path, b"").unwrap();
        let log = LogFile::open(&path).unwrap();
        assert!(tail.write_to(log.file.as_raw_fd()));
        assert_eq!(&std::fs::read(&path).unwrap()[note.len()..], &printed[printed.len() - TAIL..]);
    }

    #[test]
    fn the_file_given_is_used_wherever_nida_runs() {
        let given = OsStr::new("/tmp/show.log");
        for exe in [APP, "/Users/me/varda/target/release/varda"] {
            for goes_nowhere in [true, false] {
                assert_eq!(
                    log_file(
                        Some(given),
                        Some(Path::new(exe)),
                        Some(Path::new("/Users/me")),
                        goes_nowhere
                    ),
                    Some(PathBuf::from("/tmp/show.log"))
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_app_opened_from_the_finder_logs_in_the_users_logs_folder() {
        let home = Path::new("/Users/me");
        for exe in [
            APP,
            // Moved by macOS before it runs (App Translocation).
            "/private/var/folders/x/T/AppTranslocation/ABC/d/Nida.app/Contents/MacOS/varda",
        ] {
            assert_eq!(
                log_file(None, Some(Path::new(exe)), Some(home), true),
                Some(PathBuf::from("/Users/me/Library/Logs/Nida/nida.log")),
                "{exe}"
            );
        }
    }

    #[test]
    fn a_program_run_from_a_terminal_prints_there() {
        let home = Some(Path::new("/Users/me"));
        // The app's own program, run from a terminal (`varda --help`, the
        // command-line tool the app installs): a terminal, a pipe or a file.
        assert_eq!(log_file(None, Some(Path::new(APP)), home, false), None);
        for exe in [
            "/Users/me/varda/target/release/varda",
            "/usr/local/bin/varda",
            "/Users/me/Nida.app/varda",
            "/Users/me/Contents/MacOS/varda",
        ] {
            for goes_nowhere in [true, false] {
                assert_eq!(
                    log_file(None, Some(Path::new(exe)), home, goes_nowhere),
                    None,
                    "{exe}"
                );
            }
        }
        assert_eq!(log_file(None, None, home, true), None);
    }

    #[test]
    fn a_log_past_the_cap_starts_afresh_and_keeps_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("nida.log");
        let old = dir.path().join("nida.log.old");

        std::fs::write(&log, "small").unwrap();
        keep_small(&log, 10).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "small");
        assert!(!old.exists());

        std::fs::write(&log, "past the cap").unwrap();
        std::fs::write(&old, "older").unwrap();
        keep_small(&log, 10).unwrap();
        assert!(!log.exists());
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "past the cap");

        // No log yet: nothing to do.
        keep_small(&log, 10).unwrap();
    }
}
