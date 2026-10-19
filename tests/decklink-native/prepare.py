"""Prepare an isolated build of the actual DeckLink sources, without the GPU app.

Only scheduling priority and tracing are no-ops. The DeckLink module, its tests,
the NDI video-range conversion, build.rs and SDK files are copied verbatim.
"""
from pathlib import Path
import shutil
import sys


def prepare(destination: Path) -> None:
    root = Path(__file__).resolve().parents[2]
    destination.mkdir(parents=True, exist_ok=True)
    for directory in ("src/internal/decklink", "vendor/decklink-sdk"):
        shutil.copytree(root / directory, destination / directory, dirs_exist_ok=True)
    shutil.copyfile(root / "build.rs", destination / "build.rs")
    ndi = (root / "src/internal/ndi/mod.rs").read_text(encoding="utf-8")
    start = ndi.index("pub(crate) fn rgba_to_uyvy(")
    opening = ndi.index("{", start)
    depth = 1
    end = opening + 1
    while depth:
        depth += (ndi[end] == "{") - (ndi[end] == "}")
        end += 1
    (destination / "src/ndi_conversion.rs").write_text(ndi[start:end] + "\n", encoding="utf-8")
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

[build-dependencies]
cc = "1"
''', encoding="utf-8")
    (destination / "src/lib.rs").write_text('''#![allow(dead_code)]
#[path = "internal/decklink/mod.rs"]
pub mod decklink;
mod ndi { include!("ndi_conversion.rs"); }
mod renderer {
    pub mod presenter {
        pub fn raise_priority(_: &str) {}
        pub fn run_in_real_time(_: &str, _: std::time::Duration,
            _: std::time::Duration, _: std::time::Duration) {}
    }
    pub mod present_trace {
        pub enum Step { Taken, Presented }
        pub struct Trace;
        pub static TRACE: Trace = Trace;
        impl Trace { pub fn note_if_free(&self, _: &str, _: u64, _: Step) {} }
    }
}
''', encoding="utf-8")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]).resolve())
