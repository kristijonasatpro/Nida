# First Dynamics section

Implementation for draft PR #11, isolated on `codex/audio-settings-upstream`.
Open a source or bus gear, expand using the icon immediately left of X, then
choose **Dynamics**, below **General**. Compact settings remain unchanged.
Headphones have no compressor category. Opening/navigating does not change sound.

## Controls and signal path

Compression starts **disabled**, including in shows saved before this feature.
The embedded FFmpeg `acompressor` uses maximum-channel stereo linking, peak
detection and full wet compression, followed by host-controlled manual makeup.
Controls: threshold -60..0 dBFS, ratio 1..20, attack 0.1..2000 ms,
release 1..9000 ms, knee 0..18 dB, makeup -12..24 dB. Reset restores defaults
and disables compression. There is no auto makeup, sidechain or lookahead.
FFmpeg's attack/release numbers describe its own detector coefficients, not
guaranteed standard 63% time constants. This is not a safety limiter.

- Sources: raw PFL -> compressor -> sync delay -> fader/balance/mute/Audio Auto
  -> existing strip meter -> bus sends. Process once, regardless of send count.
- Buses: sum -> compressor -> fader/mute -> existing bus meter -> normal
  headphones monitor -> sync delay -> output consumers.
- Pre FX and Post FX show stereo **sample peaks**, before the fader, in dBFS.
  They are decaying displays, not LUFS, RMS or true-peak measurements.
- Gain reduction measures the applied linked attenuation on non-silent samples,
  excluding makeup, with peak/display decay. It is not FFmpeg's private detector
  state; silence cannot reveal that state from input/output ratios.
- Raw solo/PFL remains unprocessed. Normal bus monitoring includes bus effects.
  Legacy direct-device recording passthrough does not traverse the mixer and
  therefore does not receive these effects. Existing output clipping remains.

## Lifecycle, failures and implementation constraints

Configurations persist with source/bus UUIDs, not display names. Unrelated fader,
route and monitor edits preserve detector history. Removing/reconnecting a
source or loading a show creates fresh state. Paused/silent sources feed silence
on the existing mix clock, allowing normal release; ordinary seeks/loops do not
recreate the rack. Real transport/media listening checks remain necessary.

Graphs are prepared on the control thread outside the mix-core mutex. Parameters
are validated before commands mutate settings; corrupt persisted values revert
to disabled defaults. Graph parameter updates occur at a block boundary under
the existing mutex. Applied gain and makeup transition over 960 samples (20 ms)
when enabling, bypassing, editing, or falling back. This does not add sample
delay and is not sample-accurate automation. Parameter edits are last-write-wins.

Initialization or processing failure keeps the source audible using dry fallback,
with an error in Dynamics. Toggle off/on to retry; unrelated edits do not repeatedly
retry a failed initialization. Previously initialized processors remain warm
while bypassed, preserving detector history and avoiding first-use allocation
on re-enable, **but still incur processing CPU cost**. Never-enabled processors
do not create a filter graph.

The bridge uses fixed 48 kHz stereo / 480-frame blocks, cached source/sink
contexts, reusable packed f64 PCM storage and no automatic format converters.
The returned frame is reused as the next input, and timing/format are verified
on every block. Existing f32 mixer samples are bridged to f64 and linked gain
is applied in place with host makeup smoothing. Reduction uses one logarithm
per block rather than per sample. FFmpeg can still allocate frame metadata;
the whole mixer is not allocation-free or hard-real-time certified.

`Processor` implements Send, not Sync: its private filter contexts are borrowed
handles into its owned graph; moving the wrapper does not move graph allocations.
Every operation requires exclusive mutable access, and the mixer shares it only
through the existing MixCore mutex. No filter context escapes the wrapper.

## Reproducing validation

From `E:/Custom Apps/Nida/work/audio-improvements` in PowerShell:

```powershell
. 'E:/Custom Apps/Nida/work/env.ps1'
$env:CARGO_TARGET_DIR='E:/Custom Apps/Nida/target'
$env:CARGO_INCREMENTAL='0'
cargo test --offline --locked --no-default-features --lib audio_mix -- --test-threads=2
cargo clippy --offline --locked --no-default-features --lib --bin varda -- -D warnings
cargo build --offline --locked --no-default-features --bin varda
```

Tests exercise the static curve, stereo ratio, pre/post/reduction semantics,
makeup, dry transparency, smoothing, invalid values, initialization/runtime
failure, frame timestamps/PCM reuse, multi-send/PFL and bus monitor routing,
old-show defaults, persistence, reconnect and inspector command targeting.
Tests require the actual FFmpeg filter and fail if unavailable, not silently skip.

Windows integration validation on 20 October 2026: 93 focused mixer/UI/bus tests
pass, including 21 new regression tests. Production library/binary Clippy with
warnings denied and the debug binary build pass. The vendored wgpu-hal build
script still emits its existing macro warnings. Static-curve assertions verify
0.5 input -> approximately 0.1767767 output at a 0.125 threshold and 4:1 ratio
(9.03 dB reduction), with preserved stereo ratio. These are signal correctness
checks, not listening or whole-app performance measurements.

Isolated upstream-base validation was attempted separately. Its older
`mozjs_sys 0.140.8-2` dependency could not obtain its prebuilt Windows archive
and then required unavailable MozTools/MozillaBuild. Tests and Clippy therefore
did not complete on that base; the 93-test/Clippy/debug-build results above are
from the combined Windows integration branch, not the isolated PR branch.
The draft remains pending isolated-base CI and packaged listening checks.

The earlier standalone reference/probe measurements are in
[`tools/audio-compressor-probe`](../tools/audio-compressor-probe/README.md).
They do not measure the new UI, smoothing, full mixer, rendering or devices.
No new whole-app performance claim follows from those measurements.

Before release, listen to speech/music and transient-heavy stereo material;
exercise enable, makeup edits, bypass, pause/resume, seek/loop and device
reconnect while streaming/recording. Check clean meters and unchanged raw PFL.
Repeat with many channels while rendering. Run the same tests/build against
macOS/Linux packaged FFmpeg: this implementation is platform-neutral, but those
platforms are not validated by the Windows run. Missing `acompressor` falls back
dry visibly. Audit the actual shipped FFmpeg binary's license/build flags.
