# Practical channel EQ

Experimental EQ for upstream review in PR #11, prepared on 20–21 October 2026.
Validation contexts and remaining release checks are documented below.
All new EQ settings default to bypassed, including shows saved before EQ.

## Research decision

The Premiere reference has useful ideas: a logarithmic frequency axis, a visible
response curve, draggable band points and numerical controls. Its full feature
count is not the requirement. Start with **one high-pass, two shelves and three
parametric bell bands**, plus output trim. This covers rumble, broad tonal balance
and several focused corrections without turning the live mixer into an editor.

Adobe describes its parametric EQ as an IIR design with five intermediate bands,
shelves and pass filters. Its documentation distinguishes Q/frequency/gain
control from a fixed-frequency graphic EQ. Our design keeps those useful
parametric controls, not the complete post-production toolset.
[Adobe's EQ reference](https://helpx.adobe.com/audition/desktop/effects-reference/filter-equalizer-effects.html).

| Candidate | Decision |
| --- | --- |
| Fixed 10-band graphic EQ | Simple sliders, but fixed centres/Q are less useful for placing a precise correction; avoid shipping two EQ interfaces initially. |
| Three bells + shelves + high-pass | Selected bounded first version: adjustable frequency/gain/Q, useful tone and rumble controls. |
| Premiere-style five bells, LP, multiple slopes, extreme Q/ranges | Defer until a concrete use case justifies added controls and processing. |
| Response graph | Included, calculated from coefficients and cached on settings changes. No audio analysis or FFT. |
| Live spectrum analyzer | Deferred separately: needs metered audio capture, analysis scheduling and display smoothing, not necessary for EQ correctness. |
| New DSP library/plugin host | Not justified: FFmpeg is already embedded and has the required filters. |

Use FFmpeg's actual `highpass`, `lowshelf`, `equalizer` and `highshelf` filters,
one planar-double graph per active rack. Zero-gain bands and disabled filters
are omitted. Flat EQ and trim-only settings do not create a graph. Do not use
FIR/reverse-IIR modes for this live path: the selected `blocksize=0` adds no
intentional sample delay. FFmpeg documents its peaking/shelving controls and
the delay of nonzero reverse-IIR block sizes.
[Filter documentation](https://ffmpeg.org/ffmpeg-filters.html#equalizer).

The preview uses the standard biquad transfer functions, not a second audio
backend. Signal tests compare the plotted response with the real FFmpeg chain.
During investigation the legacy `bass`/`treble` filters did not match the chosen
monotonic shelf model; selecting `lowshelf`/`highshelf` fixed that mismatch without
relaxing the comparisons. Also, this SDK calls the option `blocksize`, despite
some documentation using `block_size`.
[FFmpeg 9 biquad implementation](https://www.ffmpeg.org/doxygen/9.0/af__biquads_8c_source.html),
[RBJ/W3C response formulae](https://webaudio.github.io/Audio-EQ-Cookbook/audio-eq-cookbook.html).

## Controls and signal flow

Gear -> expand beside X -> **Equalizer**, between General and Dynamics.
Compact settings are unchanged; headphones have no EQ category.

- Master Enable EQ and Reset EQ (reset restores flat settings and bypasses).
- Optional 12 dB/oct Butterworth high-pass, 20–500 Hz, initially off at 80 Hz.
- Low shelf, 20–2000 Hz, and high shelf, 1–20 kHz; gain -12..+12 dB,
  fixed monotonic shelf slope. Defaults: 120 Hz / 8 kHz, both at 0 dB.
- Three independently bypassable bells: 20 Hz–20 kHz, -12..+12 dB, Q 0.25–10.
  Defaults: 250 Hz, 1 kHz, 4 kHz, Q 1, all at 0 dB.
- Output trim -24..+12 dB before compression, initially 0 dB.
- Drag L/1/2/3/H points for frequency/gain; numerical controls also set Q.
  Editing a band does not silently enable the master EQ. The curve is a settings
  preview while bypassed, not a measured spectrum, and displays a ±24 dB range.
- Stereo sample-peak Pre EQ / Post EQ meters, before compressor and fader.

Sources: raw PFL -> EQ -> compressor -> input sync delay -> fader/balance/
mute/Audio Auto -> existing strip meter -> sends. EQ runs once per source,
not once per destination. Both stereo channels use the same coefficients but
independent state; EQ does not collapse or cross-feed them.

Buses: sum -> EQ -> compressor -> fader/mute -> existing meter -> normal
headphones monitor -> sync delay -> output consumers. Raw solo/PFL stays raw.
Legacy direct-device recording passthrough still bypasses the mixer entirely.

Boosts can exceed output headroom; use trim and watch the calibrated bus meters.
Neither EQ nor the existing compressor is a safety/true-peak limiter. Existing
delivery clipping remains; the graph's display range is not a peak ceiling.

## Live edits, lifecycle and failure

Graphs and frame storage are prepared on the control thread outside the mix-core
mutex. Unchanged configs, fader edits and routing changes preserve filter history.
Changed EQ settings fade the old EQ to dry over 480 samples (10 ms), swap at a
block boundary, then fade the new EQ in over 480 samples. Pending edits coalesce
to the latest settings. This deliberately passes briefly through dry audio,
rather than changing IIR coefficients abruptly or running two graphs in parallel.
New filter history starts on an actual EQ edit; listen for low-frequency changes
on sustained material. This is not sample-accurate automation.

Bypass completes the fade and retires the graph: unlike the first compressor
implementation, a bypassed EQ does not keep filtering audio. Retired/superseded
graphs are returned to the control thread for destruction after unlocking the
core. No graph construction, coefficient parsing, logging or new Rust vectors
are needed per steady-state audio block. FFmpeg frame metadata can still allocate;
neither FFmpeg nor the whole mixer is certified allocation-free/hard-real-time.

The private Processor owns its graph and cached contexts. It is Send, not Sync:
moving it leaves graph allocations in place, and access is exclusive under the
existing mixer mutex. Its context handles never escape the wrapper.

The fixed 48 kHz stereo / 480-frame bridge validates format, sample count and
timestamps, and reuses both returned PCM planes for the next block. No automatic
resampling/format converter is inserted. The existing f32 interleaved mixer is
copied to planar f64 and back; fusion with the compressor is a future measured
optimization, not a prerequisite for a safe first EQ.

The first debug core measurement exposed a default-off regression from scanning
the same samples for another pair of meters. Fully bypassed EQ now returns
immediately and uses the compressor's existing input-meter tap for its identical
pre/post readings. This keeps a new bypassed feature from adding a full meter
walk to every source and bus. Partially faded/active EQ retains its own taps.

Filter initialization or processing failures show a visible dry fallback and
leave the source audible. Toggle off/on to retry. Unrelated edits do not loop
failed initialization attempts. A runtime failure has to fall back immediately
because no valid filtered frame exists; that rare transition can click. Invalid
stored values become disabled defaults; invalid commands are rejected. Removal,
reconnect and show load reset rack history. Ordinary pause/seek/loop behavior
keeps the existing rack and feeds silence where the current mixer does.

## Validation and measurement

Signal tests use the actual filters and fail when unavailable. They cover flat
transparency, centre gain/cut, high-pass attenuation, shelves, plotted response,
stereo independence, invalid/extreme settings, release to silence, live edits,
initialization/runtime fallback, timestamps and PCM reuse. Core/engine/UI tests
cover pre-compressor ordering, multi-send/PFL, bus monitoring, persistence,
old shows, reconnect, reset and real graph-handle commands.

From the audio worktree in PowerShell:

```powershell
. 'E:/Custom Apps/Nida/work/env.ps1'
$env:CARGO_TARGET_DIR='E:/Custom Apps/Nida/target'
$env:CARGO_INCREMENTAL='0'
cargo test --offline --locked --no-default-features --lib audio_mix -- --test-threads=2
cargo clippy --offline --locked --no-default-features --lib --bin varda -- -D warnings
cargo build --offline --locked --no-default-features --bin varda
cargo test --offline --locked --manifest-path tools/audio-eq-probe/Cargo.toml
```

`mixer_block_budget_probe` is an ignored, non-real-time **production mix-core**
probe: 48 kHz stereo, 480-frame 1 kHz fixture, 1/8/16/32 sources, two sends each,
two buses, 100 warm-up and 2000 measured blocks per case. FIFO refill is outside
the block timer. It records p50/p95/p99/max service times and counts service times
above 10 ms, not real device underruns or scheduler wake-up misses. Capturing the
pre-EQ baseline before implementation used the same debug profile and fixture.
Repeat one workload at a time without concurrent builds and report that profile.

```powershell
cargo test --offline --locked --no-default-features --lib mixer_block_budget_probe -- --ignored --nocapture --test-threads=1
$binary=(Get-ChildItem "$env:CARGO_TARGET_DIR/debug/deps/varda-*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
./scripts/measure-audio-eq.ps1 -Binary $binary -Mode disabled -OutputRoot ./measurements/eq-off
./scripts/measure-audio-eq.ps1 -Binary $binary -Mode sources -OutputRoot ./measurements/eq-inputs
./scripts/measure-audio-eq.ps1 -Binary $binary -Mode sources-and-buses -OutputRoot ./measurements/eq-buses
```

The wrapper records binary SHA, process CPU seconds and sampled peak working set
where available. CPU includes fixture refill/setup; this is a busy-loop test,
**not live playback CPU utilization**. Rendering, devices, clip decoding and UI
cost are excluded. A separate release-profile probe reuses production EQ code
without the renderer/mixer and counts Rust allocations (not FFmpeg C allocations).
Do not conflate these two workloads or extrapolate them to complete shows.

## Platform and release checks

The state API exposes live EQ status/meters as `equalizer_state` on sources and
buses. A bus's flattened `equalizer` remains its persisted configuration. These
names must differ: otherwise duplicate JSON keys hide readiness/fallback status
from normal JSON consumers. A regression covers both keys and source status.

Measured results and limitations: [20 October Windows probes](audio-equalizer-measurements.md).

There are no Windows APIs in EQ itself. macOS/Linux builds still need the selected
FFmpeg filters and matching library ABI: test their actual packaged binaries,
not just the Windows SDK. Missing filters take the visible fallback path. The
chosen FFmpeg source is LGPL-2.1-or-later, but that does not relabel the already
GPL-configured Windows FFmpeg SDK; distribution must honor its complete build.
No additional DSP runtime or Faust-generated code is linked into production.

Before release, listen to speech, music and stereo sustained tones; drag controls,
bypass, reset, pause/resume, seek/loop and reconnect while recording/streaming and
rendering. Check output headroom, raw PFL and independent bus behavior. Review the
curve at small/high-DPI window sizes and regenerate platform-appropriate golden
images only after visual review; comparisons are not weakened here.
