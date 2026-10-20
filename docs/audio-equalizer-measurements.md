# EQ measurements — 20 October 2026

Windows, AMD Ryzen 9 5950X (32 logical processors), bundled FFmpeg 9 SDK.
Rust 1.99.1, x86_64-pc-windows-msvc, incremental compilation disabled.
These are **offline block-service probes**, not playback CPU utilization,
device underrun counts, or end-to-end latency. No renderer, decoding, devices,
recording or UI runs in the probes. See [design and reproduction](audio-equalizer.md).

## Production mix core, debug profile

Identical 48 kHz stereo 480-frame sine fixture, two sends per source, two buses,
100 warm-up and 2,000 timed blocks per case. FIFO refill is outside the timer.
Baseline was captured before EQ integration; its executable is preserved at
`work/audio-eq-baseline-tests.exe` (SHA256
`C010A134068A15D3E78DEFF6C64D060CE384A6FA54E6CB0DD98D186B6B20427F`).
After measurements repeat each mode three times, serially, without builds.
The table shows the baseline metadata run and final third run, in microseconds:

| Sources | Before p50 / p95 / p99 | EQ off p50 / p95 / p99 | Source + bus EQ p50 / p95 / p99 |
|---:|---:|---:|---:|
| 1 | 114.0 / 118.1 / 143.3 | 114.8 / 119.1 / 151.3 | 251.3 / 256.4 / 268.5 |
| 8 | 387.4 / 409.3 / 475.0 | 388.5 / 400.5 / 421.7 | 840.8 / 875.2 / 891.0 |
| 16 | 692.8 / 731.9 / 795.9 | 697.8 / 711.9 / 769.2 | 1607.8 / 1735.2 / 1871.8 |
| 32 | 1382.0 / 1501.4 / 1659.0 | 1401.4 / 1568.1 / 2155.0 | 3050.4 / 3271.4 / 4518.9 |

Across the three after runs, 32-source disabled p50 was 1400.5–1401.5 us;
full EQ p50 was 3050.4–3057.7 us, p99 3575.8–4521.6 us, maximum 5166 us.
Source-only EQ p50 was 2954.7–2963.0 us. All recorded cases had **zero service
times above 10 ms**. This does not establish real-time scheduling safety.
Baseline repeated p50 values were around 1382–1389 us at 32 sources. The
default-off integration is near that baseline, not a demonstrated speedup;
tail latency varied between runs.

The first implementation rescanned PCM for EQ pre/post meters while bypassed:
32-source p50 was 1844.9 us. Reusing the identical existing compressor-input
meter tap and returning immediately from fully bypassed EQ reduced that to
about 1401 us. Active EQ still scans its own input/output for useful metering.

Wrapper totals across all four source counts (busy loop, including setup/refill):

| Mode | Process CPU seconds | Sampled peak working set |
|---|---:|---:|
| Before, metadata run | 5.9375 | 21.92 MiB |
| After disabled, three runs | 5.875–6.000 | 20.54–20.56 MiB |
| After source EQ | 11.906–12.250 | 23.76–23.80 MiB |
| After source + bus EQ | 12.609–12.766 | 23.91–23.96 MiB |

Memory is a 20 ms sampled process working set, not an allocation accounting or
proof of reduced memory usage. Raw logs and binary metadata are under
`work/audio-eq-before*`, `work/audio-eq-after-disabled-*`,
`work/audio-eq-after-sources-*`, and `work/audio-eq-after-sources-and-buses-*`.

## Production EQ alone, release-profile probe

`tools/audio-eq-probe` imports the production EQ implementation directly.
Cargo's standalone release profile is optimized but is **not the app's
thin-LTO release build**. It times copying the same fixture and processing
1/8/16/32 independent EQ instances, 100 warm-up + 5,000 measured blocks each.
Every active EQ uses HP + both shelves + all three bells, with nonzero gains.
Readiness/error assertions prevent dry fallback being reported as EQ speed.

Final third run, microseconds:

| EQ instances | Disabled p50 / p95 / p99 | Enabled p50 / p95 / p99 |
|---:|---:|---:|
| 1 | 0.1 / 0.1 / 0.1 | 14.3 / 14.9 / 17.7 |
| 8 | 0.3 / 0.3 / 0.4 | 117.0 / 124.7 / 126.7 |
| 16 | 0.5 / 0.5 / 0.5 | 235.7 / 241.9 / 249.1 |
| 32 | 0.9 / 1.0 / 1.0 | 504.6 / 517.6 / 587.5 |

Across three enabled runs, 32 EQ p50 was 504.6–506.8 us, p99 587.5–652.2 us,
maximum 757.8 us. Zero service times exceeded 10 ms. The Rust allocator counter
reported zero allocations during timed steady-state processing in all cases;
it **does not count FFmpeg's C allocations** or graph preparation.
Process CPU totals were 4.453–4.531 seconds enabled versus 0.078–0.109 disabled;
sampled working set was about 17.87–17.90 MiB enabled. These separate workloads
cannot be used to infer full-app CPU percentages or compare debug/release speed.

```powershell
cargo test --offline --locked --release --manifest-path tools/audio-eq-probe/Cargo.toml --no-run
$binary=(Get-ChildItem "$env:CARGO_TARGET_DIR/release/deps/audio_eq_probe-*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
./scripts/measure-audio-eq.ps1 -Binary $binary -Test equalizer_bank_budget_probe -Mode disabled -OutputRoot ./measurements/eq-release-off
./scripts/measure-audio-eq.ps1 -Binary $binary -Test equalizer_bank_budget_probe -Mode sources -OutputRoot ./measurements/eq-release-on
```

Raw logs: `work/audio-eq-release-{disabled,sources}-{1,2,3}`. Measured release
binary SHA256: `F3CFADBF3D6AE69DFFC8329B839506D2211795F53F1AB92F130FED468999B91E`.

## Combined EQ and corrected compressor follow-up

After the bypass correction, the same production mix-core probe also runs EQ
and compression on every source and both buses. Compressor fixture: threshold
-36 dBFS, ratio 4:1, attack 10 ms, release 100 ms, makeup +3 dB. The existing
EQ and PCM fixtures, debug profile, warm-up and timed block counts are unchanged.
Every active processor must report ready with no fallback error. Run serially:

```powershell
./scripts/measure-audio-eq.ps1 -Binary $binary -Mode sources-and-buses -Compressor -OutputRoot ./measurements/eq-compressor
```

Third run, microseconds:

| Sources + 2 buses | p50 | p95 | p99 | Maximum | Service >10 ms |
|---:|---:|---:|---:|---:|---:|
| 1 | 331.1 | 371.2 | 419.6 | 564.6 | 0 |
| 8 | 1089.1 | 1123.7 | 1176.5 | 1724.7 | 0 |
| 16 | 1965.3 | 2034.6 | 2067.3 | 2936.5 | 0 |
| 32 | 3719.8 | 3861.8 | 3967.0 | 6430.6 | 0 |

Across three runs, the 32-source p50 was 3719.1–3725.8 us and p99
3967.0–4057.8 us. All cases had zero service times above 10 ms. Total busy-loop
process CPU was 15.406–15.484 seconds over all four source counts, with sampled
peak working set 24.50–24.52 MiB. This is an additional workload, **not a
before/after compressor speedup or live playback qualification**. It excludes
rendering/devices and does not count FFmpeg C allocations. Binary SHA256:
`BCB41FA7F6C73104CBF0A40F28AD7A70FC80DF7581FD18978D2D9C8A24326B2D`.
Raw results: `tools/audio-eq-probe/results/windows-compressor-eq-2026-10-20.json`.

The compressor fix is supported by four tests that fail on the old independent
ramps and pass on combined-gain interpolation. Those cover compensated bypass
and re-enable, opposing ratio/makeup edits, interrupted transitions and runtime
fallback. The separate production-module harness passes 35 signal/reader tests;
the combined fork passes 116 focused audio/UI tests (two manual probes ignored).
Historical EQ-only measurements above are retained, not relabeled as combined
processing or real-time allocation evidence.

## Remaining evidence

21 October follow-up: 117 focused tests pass after fixing a duplicate EQ JSON
key exposed by a live API check; telemetry now uses `equalizer_state`. Clippy
and the native debug build pass. A 32.4 s native smoke test runs four generated
640x360/30 clips with source/master EQ and compression, GUI rendering and
640x360/30 ProRes/stereo-PCM recording. It exercises bypass, parameter edits,
pause/resume, looping, EOF and seeks with no reported DSP fallback. Recorded
peak is -20.7403 dBFS; the only >50 ms silences below -60 dBFS match deliberate
pause/EOF. Observed render rate is 59.80 FPS with zero encoder-video drops.
This is not an audio-deadline count, subjective listening pass, long-run
qualification or C-allocation profile. Physical outputs were left unselected.
WPR heap tracing requires an administrator token unavailable in this session;
Mac goldens and packaged-platform checks remain pending.

Combined Windows fork validation: 112 focused mixer/core/engine/UI tests pass
(two manual probes ignored), the actual egui EQ screenshot test passes,
production lib/bin Clippy with `-D warnings` passes, and the native debug build
passes. The standalone signal workspace passed 20 tests. Existing vendored
wgpu-hal macro warnings remain. No golden-image comparisons were weakened or
reference images replaced.

The larger-window change initially also forced a compact default height of
zero; two existing compact-setting interaction tests caught that regression.
Height customization now applies only to expanded mode and both tests pass.

This is **combined fork** validation, not a build of isolated upstream PR #11.
The fresh 21 October isolated Windows check instead reaches app code and fails
on existing Unix-only DeckLink clock calls; the older mozjs issue was historical.
These measurements were taken before publishing the EQ follow-up.

Corrected compressor/EQ native preview: `work/audio-pr11-preview-20261020-235456`,
`nida-audio-pr11-debug.exe`, port 8112. Open the Master gear, expand beside X,
then choose Equalizer. Captures/OSC/NDI are disabled and no real show is loaded.
The real egui rendered inspection image is `work/audio-eq-ui-preview.png`;
its meter values are deterministic test fixtures, not live measurements.
The initial native UI automation approval timed out; full native interaction/listening
remains a user review step even though the process launched successfully.

Native full-app listening, rendering/streaming load, high-DPI interaction,
device behavior and packaged macOS/Linux builds remain release checks. A cheap
cached response curve is implemented; no FFT analyzer or UI-performance claim
is made. Immediate emergency dry fallback may click; ordinary bypass/parameter
changes use short fades and temporarily pass dry audio rather than run two
graphs at once.
