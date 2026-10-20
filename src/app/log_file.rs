//! Where Nida's log goes. Opened from the Finder, an app has no terminal to
//! print to. The app `scripts/make-local-app.sh` makes names a file in
//! `VARDA_LOG_FILE`; the one given away in a DMG names none, and everything
//! it printed went nowhere — its warnings, its errors, a log for the user to
//! send. So Nida's program inside an app bundle, opened where what it prints
//! goes nowhere, logs to `~/Library/Logs/Nida/nida.log` unless told
//! otherwise; run from a terminal, it prints there as any program does.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "/Applications/Nida.app/Contents/MacOS/varda";

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
