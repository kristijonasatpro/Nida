use clap::Parser;

/// Send everything the app prints — its log and ffmpeg's messages — to the
/// file `VARDA_LOG_FILE` names, or, for an app bundle on a Mac opened from
/// the Finder, to `~/Library/Logs/Nida/nida.log` (see `app::log_file`): an
/// app opened from the Finder has no terminal to print to.
#[cfg(unix)]
fn log_to_file() {
    use varda::app::log_file::{keep_small, log_file, LOG_CAP};
    let given = std::env::var_os("VARDA_LOG_FILE");
    let exe = std::env::current_exe().ok();
    let Some(path) = log_file(
        given.as_deref(),
        exe.as_deref(),
        dirs::home_dir().as_deref(),
        output_goes_nowhere(),
    ) else {
        return;
    };
    if given.is_none() {
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("{}: {e}", dir.display());
            }
        }
        // Named for what reads it later: traces are written beside the log
        // (see `renderer::present_trace::trace_path`). Nothing else runs yet.
        std::env::set_var("VARDA_LOG_FILE", &path);
    }
    if let Err(e) = keep_small(&path, LOG_CAP) {
        eprintln!("{}: {e}", path.display());
    }
    // What libraries print as well as the log, through a pipe and a writer
    // thread: none of them waits on the file (see `log_into_file`). Should
    // that not be had, nothing is pointed at the file: a show thread's print
    // would wait on it again.
    if let Err(e) = varda::app::log_file::log_into_file(&path) {
        eprintln!("{}: not logged there ({e})", path.display());
        return;
    }
    // Kept to the cap through a long day too, not only from launch to
    // launch: past it, the log moves aside and the writer starts it afresh —
    // and starts again should the file go missing.
    let keeper = std::thread::Builder::new()
        .name("log-keeper".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_CAP);
            if big && keep_small(&path, LOG_CAP).is_err() {
                continue;
            }
            if big || !path.exists() {
                varda::app::log_file::reopen();
            }
        });
    if let Err(e) = keeper {
        eprintln!("Log not kept to its size: {e}");
    }
}

/// Whether what Nida prints goes nowhere: stdout and stderr are both
/// `/dev/null`, as for an app opened from the Finder or the Dock — neither a
/// terminal, a pipe nor a file someone asked for (`varda --help > help.txt`).
#[cfg(unix)]
fn output_goes_nowhere() -> bool {
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::fs::MetadataExt;
    let Ok(null) = std::fs::metadata("/dev/null") else {
        return false;
    };
    let is_null = |fd: BorrowedFd<'_>| {
        fd.try_clone_to_owned()
            .and_then(|fd| std::fs::File::from(fd).metadata())
            .is_ok_and(|meta| meta.dev() == null.dev() && meta.ino() == null.ino())
    };
    is_null(std::io::stdout().as_fd()) && is_null(std::io::stderr().as_fd())
}

fn main() {
    // Opened from the Finder, the app would not find FFmpeg (see tool_path).
    varda::app::tool_path::find_tools_anywhere();
    #[cfg(unix)]
    log_to_file();
    // Each line goes to the log through a thread of its own: no thread that
    // logs — the control window's, the show's — waits on the file (see
    // `log_file::QueuedLog`).
    let mut logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    logger
        .filter_module("naga", log::LevelFilter::Error)
        .filter_module("egui_wgpu", log::LevelFilter::Error)
        .filter_module("ort", log::LevelFilter::Error)
        .target(env_logger::Target::Pipe(varda::app::log_file::queued_stderr()));
    // A pipe gets no colours by itself; RUST_LOG_STYLE=always or never (read
    // above) still decides, else as for stderr before.
    if !matches!(
        std::env::var("RUST_LOG_STYLE").as_deref(),
        Ok("always" | "never")
    ) {
        logger.write_style(if colours_on_stderr() {
            env_logger::WriteStyle::Always
        } else {
            env_logger::WriteStyle::Never
        });
    }
    logger.init();

    // The report goes through the log's writer too — Rust's own writes to
    // stderr, the log file, in place, and a held-up write would hold the
    // panicking thread there — whatever RUST_LOG filters out, with a
    // backtrace when RUST_BACKTRACE asks for one.
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let wanted = std::env::var_os("RUST_BACKTRACE").is_some_and(|v| v != "0");
        let trace = if wanted {
            format!("\n{}", std::backtrace::Backtrace::force_capture())
        } else {
            String::new()
        };
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let text = format!("[{now} ERROR panic] PANIC in thread '{name}': {info}{trace}\n");
        let wait = std::time::Duration::from_millis(500);
        varda::app::log_file::report(&text, wait);
        varda::app::log_file::drain(wait);
    }));

    let ran = run();
    // Said here rather than by returning it: printed after `main`, it would
    // come after the log's last lines were waited for, and be lost.
    if let Err(e) = &ran {
        eprintln!("Error: {e:?}");
    }
    // The last lines into the log before the process ends.
    varda::app::log_file::drain(std::time::Duration::from_secs(1));
    if ran.is_err() {
        std::process::exit(1);
    }
}

/// Whether text to stderr gets colours: a terminal, unless `NO_COLOR`,
/// `CLICOLOR=0` or `TERM=dumb` say otherwise; `CLICOLOR_FORCE` always.
fn colours_on_stderr() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0");
    if set("CLICOLOR_FORCE") {
        return true;
    }
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
        || std::env::var_os("CLICOLOR").is_some_and(|v| v == "0")
        || std::env::var_os("TERM").is_some_and(|v| v == "dumb")
    {
        return false;
    }
    std::io::IsTerminal::is_terminal(&std::io::stderr())
}

fn run() -> anyhow::Result<()> {
    let config = varda::app::AppConfig::parse();
    if config.bench_encoders {
        #[cfg(target_os = "macos")]
        println!("{}", varda::streaming::vt_bench::run());
        #[cfg(not(target_os = "macos"))]
        println!("The media engine bench runs on a Mac.");
        return Ok(());
    }
    if let Some(name) = &config.blackmagic_sound_lag {
        #[cfg(target_os = "macos")]
        println!("{}", varda::decklink::sync_probe::run(name));
        #[cfg(not(target_os = "macos"))]
        println!("Blackmagic boxes are measured on a Mac ({name}).");
        return Ok(());
    }

    // First-launch CLI install (non-blocking, non-fatal). An app bundle
    // built for one machine sets VARDA_NO_CLI_INSTALL: no admin prompt.
    if std::env::var_os("VARDA_NO_CLI_INSTALL").is_none() {
        varda::cli_install::ensure_cli_installed();
    }

    log::info!("Nida - Starting up...");
    if config.headless {
        log::info!("Headless mode enabled (API port {})", config.api_port);
    }

    varda::usecases::ui::runner::UIRunner::new(config).run()
}
