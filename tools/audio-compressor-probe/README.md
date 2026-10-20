# Embedded FFmpeg compressor feasibility probe

This standalone experiment does not change Nida's mixer or enable processing.
It uses the same `ffmpeg-next = 9.0.0` dependency as Nida. Missing filters fail
the experiment, rather than silently measuring a replacement processor.

## Reproduce on Windows

From the audio worktree, using the existing Windows SDK/tool setup:

```powershell
. 'E:/Custom Apps/Nida/work/env.ps1'
$env:CARGO_TARGET_DIR='E:/Custom Apps/Nida/target'
cargo test --offline --locked --manifest-path tools/audio-compressor-probe/Cargo.toml
cargo clippy --offline --locked --manifest-path tools/audio-compressor-probe/Cargo.toml --all-targets -- -D warnings
cargo build --release --offline --locked --manifest-path tools/audio-compressor-probe/Cargo.toml
& "$env:CARGO_TARGET_DIR/release/audio-compressor-probe.exe" 5000 `
  1> results.jsonl 2> graph.log
```

On another system, configure its FFmpeg development libraries normally; omit
`--offline` for the initial dependency download. The Rust harness is portable;
process CPU accounting currently uses Windows `GetProcessTimes` and is null
elsewhere. Do not substitute a system FFmpeg executable for the embedded test.

## Method

- Release build; embedded libavfilter, one graph thread, no spawned FFmpeg process.
- Stereo 48 kHz, 480-frame/10 ms blocks, independent graph per strip.
- Deterministic 997 Hz sine at three envelope levels, asymmetric stereo,
  repeating every 0.5 seconds. No media download or random fixtures.
- Identical input, pre/post peak and RMS meter work, output copies, warm-up,
  timing and build profile for all four paths. Pre-generated input and reserved
  duration storage; graph creation and 100 warm-up blocks excluded.
- `native-copy`: host copy and meters only, **not a native compressor**.
- `anull-f32`: empty FFmpeg path plus bridge and meters.
- `compressor-f32`: host f32 -> FFmpeg automatically converts to packed f64
  for acompressor -> automatically converts back to f32 -> host.
- `compressor-f64`: host converts while copying into packed f64 -> compressor
  -> host converts back to f32. Recycles the returned frame as next input.
- Threshold 0.125, ratio 4:1, attack 10 ms, release 100 ms, hard knee,
  unity makeup, maximum stereo link, peak detector, fully wet.
- Three repetitions of 5,000 aggregate blocks per path and strip count;
  each repetition represents 50 seconds of audio, processed without sleeping.
  Fixed execution order; CPU frequency, desktop load and scheduling uncontrolled.
- p50/p95/p99 measure aggregate service time for all strips, including bridges,
  assertions and metering. A missed 10 ms service budget is **not** a measured
  end-to-end playback underrun. This excludes Nida summing, devices, UI,
  recording, rendering, and OS wake-up deadlines.
- CPU time is whole-process user+kernel, not wall time. The JSON reports both
  saturated-loop CPU and CPU normalized to the represented audio duration.
  The latter is a percentage of **one logical CPU**, not the whole 32-thread
  machine; coarse process accounting makes small cases particularly noisy.
- Memory/heap allocations are not instrumented. Reusing PCM storage does not
  establish allocation-free processing: FFmpeg may allocate frame headers,
  buffer references or other internals. Do not use Rust-only allocation counts
  to make claims about FFmpeg's C allocations.

## Windows result, 20 October 2026

AMD Ryzen 9 5950X, 32 logical processors; Windows NT 10.0.26300.0;
Rust 1.99.1; FFmpeg n9.0.2-28-g669f5ab083-20261015 shared Windows SDK.
Ordinary desktop load, not a controlled rendering/playback stress run.
Raw output and negotiated graph are in `results/`.

Median of the three independently reported quantiles, microseconds per
aggregate 10 ms block (not pooled percentiles):

| Strips | Path | p50 us | p95 us | p99 us | CPU at real time, % one CPU |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | native copy + meters | 0.9 | 0.9 | 1.0 | 0.00* |
| 1 | anull f32 | 1.3 | 1.5 | 1.8 | 0.03 |
| 1 | compressor f32 | 11.3 | 11.5 | 16.4 | 0.09 |
| 1 | compressor f64 bridge | 10.3 | 10.5 | 15.1 | 0.09 |
| 8 | compressor f32 | 90.5 | 100.6 | 119.8 | 0.72 |
| 8 | compressor f64 bridge | 82.5 | 86.6 | 104.2 | 0.62 |
| 16 | compressor f32 | 182.6 | 197.6 | 221.0 | 1.47 |
| 16 | compressor f64 bridge | 165.2 | 186.4 | 213.8 | 1.31 |
| 32 | native copy + meters | 26.9 | 28.1 | 29.8 | 0.28 |
| 32 | anull f32 | 40.2 | 44.9 | 53.9 | 0.41 |
| 32 | compressor f32 | 367.8 | 399.4 | 422.4 | 2.94 |
| 32 | compressor f64 bridge | 332.2 | 363.1 | 423.0 | 2.59 |

*Below process CPU accounting resolution in this case; not zero CPU usage.

All 240,000 measured aggregate blocks met the 10 ms service budget. Worst
observed compressor-f64 block was 1,398.9 us. At 32 strips, the manual f64 bridge
reduced median p50 service time about 9.7% and normalized process CPU about 12%
relative to automatic f32 conversion, but did **not** improve median p99 in this
run. This is neither an end-to-end latency result nor a comparison against a
native Rust compressor. The native-copy baseline performs no compression.

## Decision and integration requirements

**Viable candidate for integration, not production-qualified yet.** The tested
bridge has ample isolated service-time headroom on this CPU. Prefer evaluating
the explicit f64 bridge rather than accepting two automatic conversion filters.

The tests protect steady-state threshold/ratio, stereo linking, silence,
float/double agreement, immediate frame counts/PTS, EOF, dry mix/unity ratio,
attack/release, envelope continuity for an unchanged runtime parameter,
invalid parameter rejection and pre/post meter placement. The f64 PCM storage
is reused across blocks on this build; this is not a general heap guarantee.

A concrete integration hazard: changing makeup from 1 to 2 doubles the next
sample immediately (+6.02 dB); FFmpeg's runtime command is not click-free host
automation. Smooth host makeup gain and bypass, and separately test smoothing
of threshold/ratio changes. Avoid rebuilding graphs on ordinary parameter edits.
Create/replace graphs away from the audio callback, preserve envelope state,
bound queues, and define a safe disabled/unavailable fallback.

The first Dynamics editor should show **Pre FX L/R**, **Post FX L/R** and
**Gain reduction**. Pre/post mean compressor input/output before the fader,
not the existing post-fader strip meter. Keep peak/RMS detector choices separate
from what the meters display. Deriving reduction from block RMS ratio is only a
test here: silence, makeup and parallel mix make that unsuitable as a general
instantaneous gain-reduction meter. Investigate accurate detector telemetry;
do not present a fabricated FFmpeg gain-reduction value.

References: [FFmpeg filter documentation](https://ffmpeg.org/ffmpeg-filters.html#acompressor)
and [official compressor source](https://ffmpeg.org/doxygen/trunk/af__sidechaincompress_8c_source.html).
Measurements above concern the installed SDK, not an assumed match to current trunk.

Before shipping: integration tests for routing and bypass, saved-state migration,
pause/reconfiguration/graph lifetime, device underruns under video rendering,
heap traces, speech/music listening, sustained paced playback and other supported
platforms/FFmpeg builds. This probe is not yet wired into the app or its UI.
