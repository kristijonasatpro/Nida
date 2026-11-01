//! Where to look for the programs Nida runs — FFmpeg above all, which
//! records, streams and makes test clips.
//!
//! Started from a terminal, Nida inherits the shell's `PATH`, Homebrew's
//! folder included. Opened from the Finder or the Dock, macOS gives it only
//! the system's `/usr/bin:/bin:/usr/sbin:/sbin`, where FFmpeg is not: REC and
//! STREAM then failed to start at all. The app given away in a DMG carries
//! its own `ffmpeg`, beside Nida's program (see `scripts/ci/build-macos.sh`).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Where package managers put programs: Homebrew on Apple silicon, Homebrew
/// on Intel Macs (and most hand-built installs), and the `MacPorts` folder.
const TOOL_FOLDERS: &[&str] = if cfg!(target_os = "macos") {
    &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
} else {
    &[]
};

const FFMPEG_PROGRAM: &str = if cfg!(target_os = "windows") {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// Homebrew's `ffmpeg-full`, kept apart from the plain `ffmpeg` (it is
/// keg-only): the FFmpeg built with SRT, and the one Nida links when it is
/// installed (see `.cargo/config.toml`).
const FULL_FFMPEG: &str = "/opt/homebrew/opt/ffmpeg-full/bin";

/// `current` (a `PATH`) with the package managers' folders added after it,
/// each only if it is not there already, so a folder the user put first
/// still wins — all but the folder of the FFmpeg to run, which goes first:
/// the app's own beside Nida's program, else `ffmpeg-full`'s when installed,
/// so REC and STREAM run the FFmpeg that can send SRT.
pub fn with_tool_folders(current: Option<&OsStr>) -> OsString {
    let own = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    with_folders(
        current,
        ffmpeg_folder(
            own.as_deref(),
            cfg!(target_os = "macos").then_some(Path::new(FULL_FFMPEG)),
        )
        .as_deref(),
    )
}

/// The first of `own` (the folder of Nida's program) and the optional
/// package-manager fallback with an FFmpeg executable in it.
fn ffmpeg_folder(own: Option<&Path>, full: Option<&Path>) -> Option<PathBuf> {
    own.into_iter()
        .chain(full)
        .find(|folder| folder.join(FFMPEG_PROGRAM).is_file())
        .map(Path::to_path_buf)
}

fn with_folders(current: Option<&OsStr>, first: Option<&std::path::Path>) -> OsString {
    let mut folders: Vec<PathBuf> = current
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    if let Some(first) = first {
        folders.retain(|f| f != first);
        folders.insert(0, first.into());
    }
    for &folder in TOOL_FOLDERS {
        if !folders.iter().any(|f| f == std::path::Path::new(folder)) {
            folders.push(folder.into());
        }
    }
    std::env::join_paths(folders)
        .unwrap_or_else(|_| current.map(OsStr::to_os_string).unwrap_or_default())
}

/// Add the package managers' folders to this process's `PATH`. Call first
/// thing in `main`, before any thread starts.
pub fn find_tools_anywhere() {
    let path = with_tool_folders(std::env::var_os("PATH").as_deref());
    std::env::set_var("PATH", path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(folders: &[&str]) -> OsString {
        std::env::join_paths(folders).unwrap()
    }

    fn assert_folders(path: &OsStr, expected: &[&str]) {
        assert_eq!(
            std::env::split_paths(path).collect::<Vec<_>>(),
            expected
                .iter()
                .map(|folder| PathBuf::from(*folder))
                .collect::<Vec<_>>()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_finder_launch_finds_homebrew() {
        let current = joined(&["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
        let path = with_folders(Some(&current), None);
        assert_folders(
            &path,
            &[
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/local/bin",
            ],
        );
    }

    #[test]
    fn a_terminal_path_keeps_its_order() {
        let current = joined(&["/Users/me/bin", "/opt/homebrew/bin", "/usr/bin"]);
        let path = with_folders(Some(&current), None);
        let expected = if cfg!(target_os = "macos") {
            &[
                "/Users/me/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/usr/local/bin",
                "/opt/local/bin",
            ][..]
        } else {
            &["/Users/me/bin", "/opt/homebrew/bin", "/usr/bin"][..]
        };
        assert_folders(&path, expected);
    }

    #[test]
    fn no_path_only_adds_platform_tool_folders() {
        let path = with_folders(None, None);
        if cfg!(target_os = "macos") {
            assert_folders(
                &path,
                &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"],
            );
        } else {
            assert!(path.is_empty());
        }
    }

    /// The app's own FFmpeg, beside Nida's program, comes before
    /// Homebrew's; without one, Homebrew's `ffmpeg-full`.
    #[test]
    fn the_apps_own_ffmpeg_goes_first() {
        let own = tempfile::tempdir().unwrap();
        let full = tempfile::tempdir().unwrap();
        let executable = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(ffmpeg_folder(Some(own.path()), Some(full.path())), None);
        std::fs::write(full.path().join(&executable), "").unwrap();
        assert_eq!(
            ffmpeg_folder(Some(own.path()), Some(full.path())).as_deref(),
            Some(full.path())
        );
        assert_eq!(ffmpeg_folder(Some(own.path()), None), None);
        std::fs::write(own.path().join(&executable), "").unwrap();
        assert_eq!(
            ffmpeg_folder(Some(own.path()), Some(full.path())).as_deref(),
            Some(own.path())
        );
        assert_eq!(
            ffmpeg_folder(None, Some(full.path())).as_deref(),
            Some(full.path())
        );
    }

    /// The FFmpeg with SRT comes before the plain one, wherever the plain
    /// one's folder stood.
    #[test]
    fn ffmpeg_full_goes_first() {
        let full = std::path::Path::new(FULL_FFMPEG);
        let current = joined(&["/opt/homebrew/bin", FULL_FFMPEG, "/usr/bin"]);
        let path = with_folders(Some(&current), Some(full));
        let expected = if cfg!(target_os = "macos") {
            &[
                FULL_FFMPEG,
                "/opt/homebrew/bin",
                "/usr/bin",
                "/usr/local/bin",
                "/opt/local/bin",
            ][..]
        } else {
            &[FULL_FFMPEG, "/opt/homebrew/bin", "/usr/bin"][..]
        };
        assert_folders(&path, expected);
    }
}
