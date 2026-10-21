# Windows hardware decoding — local prototype

Windows GPU decoding remains experimental and off by default. The Setup toggle
opts in without changing macOS automatic hardware selection.

FFmpeg's D3D11VA decoder can now feed the same hardware-frame download and
colour-conversion path used by VideoToolbox on macOS. Windows activation is
experimental and opt-in: open **Layout → Panels → Setup**, then enable
**Performance → Use GPU video decoding (experimental)**. The choice is saved
in `Nida/performance.json` in the user's settings folder, across shows and
restarts. It applies to videos and streams opened afterward; already-open
players keep their decoder. Unsupported videos still fall back to CPU.

`VARDA_WINDOWS_HARDWARE_DECODE=1` supplies the initial choice when there is no
saved preference. A saved choice takes precedence, so the checkbox can disable
an environment-enabled test launch too. `VARDA_SOFTWARE_DECODE` overrides both
and forces software decoding; the UI explains why its checkbox is disabled.
macOS keeps its existing automatic hardware selection.

The decoder checks the codec's advertised hardware configurations before
creating a device. Unsupported codecs and unavailable devices use the regular
multithreaded software decoder. AV1 uses FFmpeg's native decoder only when a
hardware device was attached. Clip and stream callers retain the actual
selection, and existing mid-clip failure recovery reopens software decoding.
The input's playback information reports whether frames actually came from
the media engine.

All 82 video unit tests passed with both default startup and Windows opt-in. The
hardware picture comparison explicitly required and confirmed GPU decoding
for H.264, HEVC, AV1 and 10-bit HEVC on the RTX 3060 Ti. macOS was not run on
this Windows machine.

This step does not add zero-copy textures or change recording/stream encoding.
Downloaded NV12/P010 frames still go through CPU colour conversion and upload.

The follow-up investigation, expanded measurement procedure, and remaining
hardware checks are in [windows-gpu-decoding-investigation.md](windows-gpu-decoding-investigation.md).

## Initial prototype measurements

Machine: NVIDIA GeForce RTX 3060 Ti, driver 32.0.16.1692. Synthetic 3840×2160,
30 fps H.264 clip, two seconds, produced with FFmpeg's `h264_nvenc`.

Criterion's `video_decode` benchmark measures seeking to the middle of the clip,
decoding forward and producing RGBA through Nida's `VideoPlayer`. In the dev
profile (native FFmpeg remains optimized), software took 106.60 ms and hardware
183.93 ms. Hardware seeking is slower in this case, so Windows stays opt-in.
The software case is an explicit CPU baseline using the same clip and wrapper;
this is not a before/after measurement of two Git revisions.

A separate FFmpeg continuous decode-to-RGBA run over all 60 frames used 5.281 s
of process CPU time in software and 1.890 s with D3D11VA, approximately 64% less.
Elapsed time was 0.310 s versus 0.485 s; maximum reported memory was roughly
528 MiB versus 1022 MiB. These are diagnostic CLI measurements, not application
FPS guarantees. Longer clips, several simultaneous clips, seeking, looping,
reverse playback and other GPU vendors need evaluation before enabling it by
default.

The first H.264 frame's decoded YUV samples matched byte for byte between CPU
and D3D11VA. FFmpeg's planar and semiplanar RGB converters produce different
edge interpolation, so picture-comparison tests repack the software reference
to the hardware frame's YUV packing before running the same RGBA conversion.
The existing strict image-difference threshold is retained.

To repeat the Criterion comparison, set `VARDA_VIDEO_BENCH_CLIP` to a clip and
run `cargo bench --bench video_decode` (use `--profile dev` for a quicker local
build). The benchmark refuses to label a software fallback as hardware.

## Nearby forward scrubbing

On Windows, short forward seeks (up to 0.25 seconds beyond the currently decoded picture)
now retain the decoder's reference pictures, queued frame and GPU surfaces.
They decode forward rather than flushing and starting again at a keyframe.
Backward and larger jumps, ping-pong, EOF and unavailable timestamps keep the
normal seek path. A decoder reopened in software always starts from a keyframe.
A repeated paused seek to the same picture delivers it once and then holds.
macOS (both software and VideoToolbox) and Linux retain the existing keyframe
seek path. The Windows results below do not validate changing seeking on Macs.

A separate Criterion baseline was recorded before this change. The same 4K
clip was scrubbed forward one frame at a time, with a backward reset every
eight steps. In the dev profile, mean estimates changed as follows:

| Decoder | Before | After |
| --- | ---: | ---: |
| Software | 99.52 ms | 18.05 ms |
| D3D11VA | 126.05 ms | 42.06 ms |

This is a scoped improvement to nearby forward scrubbing, not arbitrary seek
latency or an application FPS claim. CPU decoding still has lower latency in
this comparison; Windows hardware decoding remains opt-in. The benchmark's
coarse-seek case now alternates positions so it cannot accidentally measure a
repeated request for the same cached picture.

Four focused checks passed: forward/repeated seek and pause/resume correctness
on both decoder requests, keyframe recovery after hardware fallback, the
existing seek-to-frame pixel check, and forward-offset playback. Production
library/binary and benchmark Clippy passes. GPU changes remain local.

## Upstream review follow-up

The forward-seek optimization is restricted to Windows because its measurements
and playback validation were on Windows. This preserves keyframe seeking on
macOS with software and VideoToolbox, and on Linux. Windows' optimization and
opt-in decoding preference are unchanged. Fallback regression coverage remains
active on every platform; only the retained-frame seek test is Windows-specific.

Cloud validation used an isolated crate compiling the production video,
transport, timebase and value modules with FFmpeg 7.1.5. It copies the production
`ResolvedModulation` value type and uses `wgpu-types` for texture formats; it
does not include application rendering or audio backends. The new non-Windows
seek regression fails without the platform restriction and passes with it.
The broader run passed 215 tests, with one ignored; after extending fallback
coverage to non-Windows, both the seek and fallback tests passed again.
This validates Linux software decoding and fallback, not D3D11VA or VideoToolbox.

Focused rustfmt passes. Strict isolated Clippy is blocked by existing
`doc_markdown`, `similar_names` and `assert_is_empty` findings in unchanged code.
Full application validation remains pending: this container lacks application
dependencies including ALSA development files and CMake; offline resolution also
stops at the uncached `arc-swap` dependency.

On configured native Windows and Mac build machines, with `ffmpeg` on PATH
(use the minimal feature set to avoid unrelated optional capture/depth SDKs):

```sh
cargo test --no-default-features --lib nearby_forward_scrubbing -- --nocapture
cargo test --no-default-features --lib hardware_fallback_restarts_at_a_keyframe_after_a_seek -- --nocapture
cargo test --no-default-features --lib the_media_engine_decodes_the_same_picture_and_does_the_work -- --nocapture
cargo test --no-default-features --lib
cargo test --no-default-features --test ui_integration
cargo clippy --no-default-features --all-targets -- -D warnings
cargo fmt --all -- --check
```

On Windows, set `VARDA_REQUIRE_HW_DECODE=1` for the media-engine test when the
GPU supports all its codecs. Check the reported backend rather than counting a
software fallback as hardware success. On a Mac, check both forced software and
actual VideoToolbox playback manually: paused and repeated scrubbing, resume,
backward/large seeks, variable-frame-rate clips, ping-pong, EOF, unsupported
codecs, and reopening in software after a failure. Confirm Windows retains its
nearby-seek improvement with decoding both enabled and disabled. These changes
do not imply upstream has accepted Windows support.

## FFmpeg build requirement

FFmpeg 9+ is now the supported build baseline. Linux CI selects a verified
FFmpeg 9 shared SDK instead of Ubuntu 24.04's FFmpeg 6.1 packages; Windows
Clippy uses the same setup as the Windows package build. macOS checks require
FFmpeg 9+ from Homebrew. The frame cleanup API and the Windows download-reuse
optimization are retained. See [source build instructions](01-getting-started.md#build-from-source).

The newer Windows download-reuse, software recovery and benchmark changes from
fork main are included in this PR as well. Existing dated Windows measurements
are historical results; changing the Linux build SDK does not validate Windows
or macOS hardware playback.

With the selected Linux FFmpeg 9 SDK, 221 production video/transport/timebase
tests and 35 compressor/EQ/reader tests passed (one ignored in each suite).
The isolated video harness excludes the two explicit GPU benchmark modules;
these runs do not validate GPU playback. Installer checks also covered a bad
checksum, preserving an unrelated directory, and reusing an installed SDK.
Workflow actionlint, YAML parsing, shell syntax and focused Rust formatting pass.
Full application and native Windows/macOS checks remain CI/hardware work.
