"""Prepare an isolated build of the actual DeckLink sources, without the GPU app.

Only scheduling priority and tracing are no-ops. The DeckLink module, its tests,
the NDI video-range conversion, build.rs and SDK files are copied verbatim.
"""
from pathlib import Path
import shutil
import sys
import tomllib


def prepare(destination: Path) -> None:
    root = Path(__file__).resolve().parents[2]
    # Match the application instead of silently testing a newer cc API.
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    cc_version = next(package["version"] for package in lock["package"] if package["name"] == "cc")
    destination.mkdir(parents=True, exist_ok=True)
    for directory in ("src/internal/decklink", "vendor/decklink-sdk"):
        shutil.copytree(root / directory, destination / directory, dirs_exist_ok=True)
    shutil.copyfile(root / "build.rs", destination / "build.rs")
    shutil.copyfile(root / "clippy.toml", destination / "clippy.toml")
    ndi = (root / "src/internal/ndi/mod.rs").read_text(encoding="utf-8")
    start = ndi.index("pub(crate) fn rgba_to_uyvy(")
    opening = ndi.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (ndi[end] == "{") - (ndi[end] == "}")
        end += 1
    (destination / "src/ndi_conversion.rs").write_text(ndi[start:end] + "\n", encoding="utf-8")
    audio = (root / "src/internal/audio/mod.rs").read_text(encoding="utf-8")
    chunk_start = audio.index("pub struct PcmChunk {")
    chunk_end = audio.index("\n}", chunk_start) + 2
    (destination / "src/pcm_chunk.rs").write_text(audio[chunk_start:chunk_end] + "\n", encoding="utf-8")
    (destination / "Cargo.toml").write_text('''[package]
name = "nida-decklink-check"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
anyhow = "1"
log = "0.4"
serde = { version = "1", features = ["derive"] }
utoipa = "5"
libc = "0.2"
crossbeam-channel = "0.5"

[build-dependencies]
cc = "1"
'''.replace('cc = "1"', f'cc = "={cc_version}"'), encoding="utf-8")
    # Apply the production module's actual pedantic lint policy too.
    manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
    lint_start = manifest.index("[lints.clippy]")
    lint_end = manifest.index("\n[dependencies]", lint_start)
    with (destination / "Cargo.toml").open("a", encoding="utf-8") as output:
        output.write("\n" + manifest[lint_start:lint_end] + "\n")
    (destination / "src/lib.rs").write_text('''#![allow(dead_code)]
#[path = "internal/decklink/mod.rs"]
pub mod decklink;
mod audio { include!("pcm_chunk.rs"); }
mod ndi { include!("ndi_conversion.rs"); }
mod renderer {
    pub mod presenter {
        pub fn raise_priority(_: &str) {}
        pub fn run_in_real_time(_: &str, _: std::time::Duration,
            _: std::time::Duration, _: std::time::Duration) {}
        #[cfg(target_os = "macos")]
        pub fn real_time_of(_: &std::thread::JoinHandle<()>) -> Option<(
            std::time::Duration, std::time::Duration, std::time::Duration)> { None }
    }
    pub mod present_trace {
        pub enum Step { Taken, Presented }
        pub struct Trace;
        pub static TRACE: Trace = Trace;
        impl Trace {
            #[allow(clippy::unused_self)] // Deliberate tracing no-op in this harness.
            pub fn note_if_free(&self, _: &str, _: u64, _: Step) {}
        }
    }
}
''', encoding="utf-8")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]).resolve())
