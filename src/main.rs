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
    if print_to(&path) {
        // Kept to the cap through a long day too, not only from launch to
        // launch: past it, the log moves aside and printing starts afresh —
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
                    print_to(&path);
                }
            });
        if let Err(e) = keeper {
            eprintln!("Log not kept to its size: {e}");
        }
    }
}

/// Point stdout and stderr at the end of the file at `path`; whether they
/// go there now.
#[cfg(unix)]
fn print_to(path: &std::path::Path) -> bool {
    use std::os::fd::AsRawFd;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => {
            let fd = file.as_raw_fd();
            // SAFETY: fd is an open file; dup2 onto stdout/stderr replaces
            // them for the rest of the process, and the file may then close.
            let (out, err) = unsafe {
                (
                    libc::dup2(fd, libc::STDOUT_FILENO),
                    libc::dup2(fd, libc::STDERR_FILENO),
                )
            };
            if out == -1 || err == -1 {
                let e = std::io::Error::last_os_error();
                eprintln!("VARDA_LOG_FILE {}: {e}", path.display());
                return false;
            }
            true
        }
        Err(e) => {
            eprintln!("VARDA_LOG_FILE {}: {e}", path.display());
            false
        }
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

fn main() -> anyhow::Result<()> {
    // Opened from the Finder, the app would not find FFmpeg (see tool_path).
    varda::app::tool_path::find_tools_anywhere();
    #[cfg(unix)]
    log_to_file();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .filter_module("naga", log::LevelFilter::Error)
        .filter_module("egui_wgpu", log::LevelFilter::Error)
        .filter_module("ort", log::LevelFilter::Error)
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("PANIC: {info}");
        default_hook(info);
    }));

    let config = varda::app::AppConfig::parse();

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
