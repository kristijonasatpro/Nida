# Windows development

Windows support is being validated. Start with the existing shared engine and
the Windows screen/window capture backend; optional features can be enabled
after this baseline builds. This is a development setup, not a claim that the
Windows application is ready for a live show.

## Build prerequisites

- Stable Rust for `x86_64-pc-windows-msvc`, including rustfmt and Clippy.
- Visual Studio C++ Build Tools with the Windows SDK. Use an x64 Developer
  PowerShell so the compiler and SDK are available.
- CMake on `PATH` (shaderc is built from source).
- libclang, with `LIBCLANG_PATH` pointing to the folder containing
  `libclang.dll`.
- An FFmpeg development distribution compatible with `ffmpeg-next` 9, including
  headers, MSVC import libraries, DLLs, `ffmpeg.exe`, and `ffprobe.exe`.
  An executable-only FFmpeg download is insufficient for linking the app.

Set paths for the current shell; substitute your own installation folders:

```powershell
$env:FFMPEG_DIR = 'C:\tools\ffmpeg'
$env:LIBCLANG_PATH = 'C:\tools\llvm\bin'
$env:PATH = "$env:FFMPEG_DIR\bin;$env:PATH"
```

`FFMPEG_DIR` must contain `include` and `lib`. Keep the runtime DLLs and tools
from that same distribution on `PATH`, so the linked libraries and subprocess
tools match. This setup uses `FFMPEG_DIR` directly rather than requiring FFmpeg
at the packaging script's default vcpkg location.

## Build and run the baseline

```powershell
cargo check --locked --no-default-features --features screen-capture
cargo build --locked --no-default-features --features screen-capture
cargo run --locked --no-default-features --features screen-capture
```

Use the same feature selection for validation:

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --no-default-features --features screen-capture -- -D warnings
cargo test --locked --no-default-features --features screen-capture --lib
cargo test --locked --no-default-features --features screen-capture --test ui_integration
```

Recording and streaming integration tests also require working FFmpeg tools and
their codecs. Screen-capture smoke tests need an interactive Windows desktop;
automated compilation does not validate cameras, audio devices, or displays.

The optional `html` and `face-detection` features are excluded from the initial
baseline, not removed from the project. The existing Windows packaging profile
enables them. The `depth` feature requires native libfreenect and is omitted on
Windows, as in the existing packaging and Clippy workflows.

## Porting conventions

Keep switching, rendering, audio mixing, show formats, UI, and API logic shared.
Put OS-specific calls behind existing platform interfaces. Validate Windows
changes alongside the existing macOS/Linux checks, and keep fixes focused so
they can be contributed upstream independently.

## Validation snapshot (2026-10-16)

With Rust 1.99.1, FFmpeg 9 shared libraries, and `screen-capture` enabled on
Windows x64:

- Native development build and core application Clippy check passed.
- Seven focused FFmpeg lifecycle, tool-discovery, and progress-parser tests passed.
- All 87 `ui_integration` tests passed.
- The headless engine served its health/state API, recorded a five-second
  1920x1080 30 fps H.264 MP4 with an AAC track, and stopped cleanly. ffprobe
  confirmed the resulting streams and duration. Device capture was disabled
  during this smoke test.

The complete library suite stopped progressing during GPU-heavy tests and was
terminated; it has not passed on Windows. Repository-wide formatting checks
find existing differences, and all-target Clippy encounters existing
`assert_is_empty` diagnostics in integration tests with this Rust version.
The passing core lint command is:

```powershell
cargo clippy --locked --lib --bin varda --no-default-features --features screen-capture -- -D warnings
```

Real camera/audio capture, desktop capture, multi-monitor presentation, NDI,
optional features, and long-running stability still need validation. The
macOS-specific app-audio, hardware media, and Blackmagic implementations have
not been ported by these baseline fixes.
