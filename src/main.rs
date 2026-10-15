use clap::Parser;

/// Send everything the app prints — its log and ffmpeg's messages — to the
/// file `VARDA_LOG_FILE` names, when it names one: an app opened from the
/// Finder has no terminal to print to.
#[cfg(unix)]
fn log_to_file() {
    use std::os::fd::AsRawFd;
    let Some(path) = std::env::var_os("VARDA_LOG_FILE") else {
        return;
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => {
            let fd = file.as_raw_fd();
            // SAFETY: fd is an open file; dup2 onto stdout/stderr replaces
            // them for the rest of the process, and the file may then close.
            unsafe {
                libc::dup2(fd, libc::STDOUT_FILENO);
                libc::dup2(fd, libc::STDERR_FILENO);
            }
        }
        Err(e) => eprintln!("VARDA_LOG_FILE {}: {e}", path.to_string_lossy()),
    }
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
