//! Opt-in diagnostic trace. No picture data, scheduling changes or counter resets.
//! Rows are [monotonic ns, thread id, event, a, b]; event fields are documented
//! in docs/windows-video-frame-pacing.md. A bounded buffered file keeps tracing
//! usable in the app as well as the release harness. Unset costs one cached check.
use std::{
    fs::File,
    io::{BufWriter, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::Instant,
};

struct Trace {
    origin: Instant,
    writer: Mutex<BufWriter<File>>,
}
static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
struct TraceThread(u64);
impl Drop for TraceThread {
    fn drop(&mut self) {
        flush();
    }
}
thread_local! { static THREAD: TraceThread = TraceThread(NEXT_THREAD.fetch_add(1, Ordering::Relaxed)); }

fn trace() -> Option<&'static Trace> {
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("NIDA_VIDEO_PACING_TRACE")?;
            // Refuse to replace evidence from an earlier run.
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(path)
                .expect("create fresh video pacing trace");
            Some(Trace {
                origin: Instant::now(),
                writer: Mutex::new(BufWriter::with_capacity(65536, file)),
            })
        })
        .as_ref()
}

#[cfg(target_os = "windows")]
pub(crate) fn enabled() -> bool {
    trace().is_some()
}

pub(crate) fn emit(event: &'static str, a: f64, b: f64) {
    let Some(trace) = trace() else {
        return;
    };
    let ns = trace.origin.elapsed().as_nanos();
    let thread = THREAD.with(|id| id.0);
    if let Ok(mut writer) = trace.writer.lock() {
        writeln!(writer, "[{ns},{thread},\"{event}\",{a:.9},{b:.9}]")
            .expect("write video pacing trace");
    }
}

pub(crate) fn flush() {
    if let Some(Some(trace)) = TRACE.get() {
        trace
            .writer
            .lock()
            .unwrap()
            .flush()
            .expect("flush video pacing trace");
    }
}
