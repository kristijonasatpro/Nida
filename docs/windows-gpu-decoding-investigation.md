# Windows decoding investigation (2026-10-20)

Windows GPU decoding stays experimental and off by default. The follow-up was
first developed and measured locally, then integrated into the Windows fork's
`main` at the user's request. PR #6's branch remains unchanged. The isolated PR checkout is
`work/gpu-upstream` at `1692d87`; the Windows integration checkout starts at
`bd65bbe`. Their original video, stream, benchmark and decoding documentation
files were identical. No installer, recording, background-effect, or other fork
changes belong in the upstream patch.

## What the code does

The decoder queues work and retains two decoded pictures. Download blocks on
the device and copies NV12/P010 into RAM. swscale converts to RGBA (RGBA64 plus
tone mapping for HDR). The player packs/rotates the output into `frame_data`;
the decode worker copies it into a recycled pool buffer; the renderer uploads
that buffer. Ping-pong additionally caches RGBA pictures within its existing
memory budget. Normal forward playback has no such cache.

Timing decode calls alone does not time GPU decoding: asynchronous work can be
charged to the later download wait. The download timer combines synchronization,
transfer, allocation and property copying; it cannot attribute each separately.
The conversion timer includes swscale and its configuration. The packing timer
includes rotation, tone mapping, deinterlacing and the worker copy, as applicable.
No inference about GPU engine occupancy follows from these CPU wall timers.

The original download unreferenced its destination every frame, requesting new
pixel storage. Windows now keeps uniquely owned storage keyed by software pixel
format and allocated surface dimensions, reallocating on layout changes or
shared buffers. The allocation uses padded hardware dimensions while conversion
receives visible dimensions. Side data and metadata are cleared before copying
new properties, preventing stale HDR information or accumulating side data.
Property-copy allocation failures are propagated. Non-Windows downloads keep
their original allocation policy.

A separate correctness bug: conversion/download failure reopened the software
decoder but reported conversion success, allowing the previous RGBA picture to
be returned for the new position. Recovery now obtains the replacement picture
before returning it, including while paused. Software errors still propagate.
The downloaded buffer is released on fallback. No converter, image-comparison
threshold, colour matrix/range handling, HDR tone map, or fallback is removed.

## Reproduction

Use one checkout/build profile, FFmpeg runtime, fixture set, driver and power
configuration for both revisions. Finish all builds before measuring. Preserve
the original executable before changing production code. Retain both source
diffs and the executables; the runner records SHA-256 of every executable and
clip, ffprobe metadata and the checkout revision. An uncommitted diff is not
identified by the revision alone. Alternate before/after execution order to
detect frequency, thermal and background-load drift.

```powershell
# Configure the normal Windows Rust/MSVC/FFmpeg development environment first.
./scripts/make-video-fixtures.ps1 -Seconds 20
cargo test --locked --no-default-features --lib --no-run --message-format=json > work/video-build.jsonl
# Find the compiler-artifact record with profile.test=true and target.name=varda;
# its executable is the test binary. Copy it to work/video-before.exe or
# work/video-after.exe before building the other revision.
./scripts/measure-video.ps1 -TestBinary work/video-before.exe -Clip tests/media/video-performance/h264-4k60.mp4 -Output work/before-4k60.log -Hardware
./scripts/measure-video.ps1 -TestBinary work/video-after.exe -Clip tests/media/video-performance/h264-4k60.mp4 -Output work/after-4k60.log -Hardware
./scripts/measure-video.ps1 -TestBinary work/video-after.exe -Clip tests/media/video-performance/h264-4k60.mp4 -Output work/after-upload.log -Hardware -Upload
./scripts/measure-video.ps1 -TestBinary work/video-after.exe -Clip tests/media/video-performance/h264-4k60.mp4 -Output work/after-playback.log -Hardware -Playback -Clips 3 -Seconds 20
```

Repeat every case with the before executable and without `-Hardware` as a CPU
control. Run 1080p60, 4K30 and 4K60, then HEVC, AV1, 10-bit HEVC and HDR-tagged
fixtures. The generator's HDR pattern tests tagged processing, not realistic HDR
mastering quality. Also use real camera/screen recordings, HLG, full range,
BT.601, VFR, nonzero start timestamps, and long-GOP clips. Reuse the files rather
than regenerating between revisions. Preserve fixture hashes. Use the same
optimized profile for shipping-performance conclusions; dev-profile numbers
only establish behavior in that profile.

The expanded benchmarks are ignored unit-test entry points so they can access
the actual pipeline stages without adding production timers or public APIs.
`measure-video.ps1` fails on missing measurements, decode errors or hardware
fallback. Every frame in the unpaced pipeline must really be a hardware frame
when hardware is required. The playback benchmark audits the actual backend of
every produced picture inside the decode worker, before publishing it. Decode
errors or fallback fail the run, including at its end. The existing Criterion
seek/scrub benchmark now checks for fallback on every measured request and
explicitly announces unavailable hardware cases.

The pipeline entry point decodes every frame to EOF without transport pacing or
seeking, with no frame dropping. Its FPS is capacity, not presented playback
FPS. p50/p95 describe per-frame CPU service time, including cold initialization
of lazily allocated resources; codec/device opening and renderer initialization
are excluded. `over_frame_budget` counts service times exceeding
the source frame period; it is not a measured display-drop count. Optional
upload includes submit and a GPU completion wait per frame, with no effect
rendering; it deliberately prevents unbounded asynchronous upload queues.

The real-time entry point uses the application's background decode workers,
buffer pool and latest-frame handoff. After one second of warm-up it polls at
the source rate, uploads all new pictures and blits every clip to an offscreen
target on each render tick. It loops clips. It reports deliveries and empty
output ticks and worker-produced pictures per clip, render deadline misses, and p50/p95 **render tick service
time**, which excludes asynchronous decoding. Empty ticks can include scheduling
jitter; deliveries cannot establish exact source-frame drops because the worker
handoff currently carries no PTS. This is a bounded rendering load, not a full
show or swapchain/presentation benchmark. Test the real application separately.

Both entry points report accumulated process CPU time and sampled RSS (every 30
frames/ticks). `cpu_core_percent` is CPU seconds / elapsed seconds * 100, so
multithreaded software decoding can exceed 100%. These are different from frame
latency and throughput. Sampled RSS is not a guaranteed peak or GPU VRAM usage.
The download address count is a reuse diagnostic, not an allocation counter.

## Regression checks

```powershell
$env:VARDA_REQUIRE_HW_DECODE = '1'
cargo test --locked --no-default-features --lib internal::video:: -- --test-threads=1
Remove-Item Env:VARDA_REQUIRE_HW_DECODE
cargo test --locked --no-default-features --lib internal::stream:: -- --test-threads=1
cargo clippy --locked --no-default-features --lib --bin varda --bench video_decode -- -D warnings
```

New checks cover conversion failure during a retained paused seek, EOF/backward/
distant/boundary scrubbing against fresh exact seeks, byte-exact active YUV rows
against independent fresh downloads, stable storage, shared-buffer protection,
pixel-format/resolution changes, timestamps, range/matrix/primaries/transfer and
replacement/removal of HDR side data. Picture comparison keeps its strict
threshold and adds full-range H.264 and PQ-tagged 10-bit HEVC. Hardware-required
tests now fail on missing fixture encoders instead of silently passing.

## Measured results

Machine: Windows, Ryzen 9 5950X (32 logical CPUs), RTX 3060 Ti (8 GiB), NVIDIA
616.92. Linked FFmpeg: n9.0.2-28-g669f5ab083-20261015. Rust 1.99.1,
`--no-default-features`, unoptimized test profile with line-table debug info.
Do not extrapolate these measurements to release builds. The rendering tests
use the RTX 3060 Ti through wgpu's Vulkan backend, alongside D3D11VA decoding.

The three fixed fixtures are six-second H.264/Main, YUV420P, synthetic testsrc2
clips, generated once with `C:/ffmpeg/bin/ffmpeg.exe` (2025-01-08 git-251de1791e)
using `-c:v h264_nvenc -preset p4 -g <2*fps> -bf 2 -pix_fmt yuv420p`. They have
360, 180 and 360 frames respectively. Exact commands, hashes and all measurement
records accompany this report. The 20-second, CPU-encoded fixtures from the
generator above are a further validation set, not the inputs to these tables.
The generator was smoke-tested at one second per clip. It verifies the encoded
codec, dimensions, rate, frame count, pixel depth and colour tags with ffprobe.
Required-hardware pipeline smoke checks also decoded all 60 frames of the
one-second 1080p60 HEVC10, PQ-tagged HEVC10 and 10-bit AV1 fixtures.
Source frames use `setparams`: on this FFmpeg build, encoder colour options alone
did not preserve transfer/primaries. The HDR picture regression explicitly
requires the tone-mapping path and RGBA64 intermediate, avoiding a mislabeled
SDR fixture passing as HDR coverage. Scaling and colour tagging share one filter
chain so a later `-vf` cannot replace the tags.

Raw data and executable/fixture/source hashes are in
[the JSONL measurement records](measurements/windows-video-2026-10-20.jsonl).
Each line is a JSON object. Earlier exploratory logs are retained locally but
are excluded from this dataset and these comparisons.

Unpaced pipeline, **no upload**; medians of three interleaved before/after runs:

| D3D11VA case | Capacity FPS before → after | p50 ms before → after | p95 ms before → after | Batch CPU ms before → after |
| --- | ---: | ---: | ---: | ---: |
| 1080p60, 360 frames | 133.17 → 140.00 | 7.39 → 7.03 | 7.75 → 7.41 | 2781 → 2656 |
| 4K30, 180 frames | 32.94 → 33.95 | 30.12 → 29.31 | 31.24 → 29.83 | 5484 → 5312 |
| 4K60, 360 frames | 32.50 → 33.56 | 30.56 → 29.61 | 32.00 → 30.73 | 11078 → 10688 |

Software control capacity FPS was 853.36 → 845.86, 120.14 → 119.91 and
113.83 → 116.93 respectively. Control drift reaches about 3%, so the modest
hardware timing differences warrant caution. In the 4K60 hardware case,
download-phase time decreased from about 10.10 to 9.17 ms/frame, while colour
conversion stayed about 16.1 ms and packing plus worker copy about 4.2 ms.
This supports retaining download storage, but does not isolate allocation cost
from synchronization and transfer. GPU decode completion can be charged to the
download phase. Why NV12 conversion is slower than planar software conversion
in this build needs profiling; a particular SIMD or driver bottleneck is not
established by these timers.

Download storage had one address after reuse, versus 16–17 distinct addresses
before. Source inspection establishes per-frame allocation before; address
counts alone cannot count allocations. Sampled RSS was effectively unchanged:
about 114 MiB at 1080p and 252 MiB at 4K for hardware. No RSS reduction is claimed.
All 360 4K60 hardware frames exceeded the 16.67 ms frame budget before and after;
reuse does not make this CPU-conversion path suitable for sustained 4K60.

A separate single before/after 4K60 hardware run with upload, submit and GPU
completion wait reached 25.99 → 26.75 capacity FPS, p95 40.14 → 38.24 ms.
Upload/wait itself averaged 7.30 → 7.24 ms/frame. It is a serialized diagnostic,
not a rendering or presentation FPS claim.

These H.264 fixtures do **not** demonstrate lower CPU cost for hardware decoding
versus software. After-change unpaced CPU time was 2656 versus 1297 ms at 1080p,
5312 versus 3485 ms at 4K30, and 10688 versus 7297 ms at 4K60. This differs from
the older standalone FFmpeg CLI result in the prototype document; the clips,
pipeline and measurement method differ. Lower real-time CPU use while delivering
fewer pictures must not be presented as an equal-throughput CPU saving.

Real-time tests use identical unoptimized, test-only source harnesses for before
and after, linked to the same integration renderer/dependencies. The baseline
video sources are extracted from `bd65bbe` (identical to PR #6's video sources);
the per-frame audit and rendering benchmark are identical in both. Local
reproduction helpers are `work/build-video-harness.ps1` and
`work/run-video-audited-playback.ps1`. Normal library test binaries can run the
same entry point using the reproduction commands above. The original-code
fallback regression harness additionally has the same test-only fault hook as
the candidate. No production fix is applied to its decoder.

Each real-time run measures ten seconds after one second of warm-up, including
looping the six-second clip. Hardware counts equal produced-picture counts in
every hardware run; no software fallback is included. Single-clip results:

| Case | Pictures delivered before → after | Render ticks | CPU core % before → after |
| --- | ---: | ---: | ---: |
| 1080p60, software | 591 → 569 | 600 | 44.1 → 44.7 |
| 1080p60, D3D11VA | 598 → 598 | 600 | 60.9 → 60.3 |
| 4K30, software | 298 → 298 | 300 | 95.5 → 96.1 |
| 4K30, D3D11VA, median of three runs | 293 → 297 | 300 | 110.8 → 107.5 |
| 4K60, software | 596 → 596 | 600 | 181.2 → 180.3 |
| 4K60, D3D11VA | 257 → 274 | 600 | 116.4 → 118.3 |

The three 4K30 hardware deliveries were 241/293/297 before and 298/297/297
after. Later baseline runs approached the candidate; the first comparison would
have overstated the benefit. Other rows are single trials and are exploratory.
The 1080p software workers produced 601 pictures in both runs while delivery
varied, showing that renderer deliveries and decode capacity are different
measurements. Initial and repeated trials also show phase/scheduling variance.
No statistically established improvement in sustained playback is claimed.

Three-clip runs are also in the raw records, with per-clip production,
delivery, sampled RSS, render deadline misses and p50/p95 render-service time.
They exercise the actual worker pool and handoff beside GPU rendering. Neither
decoder consistently delivers the requested rates in all three-clip scenarios.
This requires further investigation of pacing, mailbox overwrites, memory
bandwidth and renderer load; these are hypotheses, not established causes.

Validation: original video baseline 157 passed, two ignored (including the new
pipeline measurement entry point). Final integration video suite 162 passed,
three ignored, with required hardware and all six codec/colour fixtures actually
on the engine at 320×180/640×360, including asserted PQ tone mapping. The stale-picture regression was run against an original-code
harness and failed, then passed with the fix. All 34 stream tests passed outside
the sandbox; five network/fixture tests had failed under sandbox restrictions.
Production library/binary/seek-benchmark Clippy passed. Broader test Clippy is
blocked by existing `assert_is_empty` lint failures in unrelated integration tests.

## Remaining checks

The isolated upstream checkout still has its pre-existing Windows
`libc::clock_gettime` / `CLOCK_THREAD_CPUTIME_ID` build failure in DeckLink;
validate the Windows integration separately rather than adding unrelated
portability fixes to this patch. macOS, other GPU vendors/drivers, release
profiles, device loss, malformed media, network-stream reconnection, HLG and
long multi-clip shows still need dedicated checks. No zero-copy or new HDR
display support is implemented. A synthetic benchmark cannot establish general
D3D11VA superiority or justify enabling it by default.

## Windows fork integration validation

On 20 October 2026, the follow-up was integrated on fork `main` starting from
`1ed9055`, together with the vertical Properties/effects layout. The existing
DeckLink audio, output, keep-awake and CI changes were retained unchanged.
The channel cards use the shared effect controls so background-loading and
failure indicators remain visible; an additional UI regression checks both
states and removing a failed effect.

The final combined sources passed 17 focused Properties/drag-and-drop/Setup
tests, 162 video tests with hardware required (three ignored), and all 34
stream tests. Production library/binary/seek-benchmark Clippy also passed with
the portable package's `screen-capture` feature enabled, as did focused Rust
formatting and whitespace checks. Commands, with the usual Windows MSVC/FFmpeg environment:

```powershell
cargo test --locked --no-default-features --lib -- usecases::ui::panels::effects::tests usecases::ui::panels::dnd::tests usecases::ui::panels::right_panel::tests --test-threads=1
$env:VARDA_REQUIRE_HW_DECODE = '1'
cargo test --locked --no-default-features --lib internal::video:: -- --test-threads=1
Remove-Item Env:VARDA_REQUIRE_HW_DECODE
cargo test --locked --no-default-features --lib internal::stream:: -- --test-threads=1
cargo clippy --locked --no-default-features --features screen-capture --lib --bin varda --bench video_decode -- -D warnings
```

These are integration regression results, not new playback performance
measurements. The earlier measurement dataset and source hashes identify the
measured pre-integration sources; they do not establish release-build or
DeckLink-hardware performance for the combined fork. GPU decoding remains off
by default and the upstream PR branch is unchanged.
