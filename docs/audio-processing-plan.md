# Audio processing: design and implementation plan

Status in Nida (21 October 2026): the first stage is in, as **Processing**:
low cut → leveler → compressor on an input, compressor → limiter and loudness
meter on a bus — see [the audio mixer chapter](16-audio-mixer.md#processing).
It runs on Nida's own DSP (`src/internal/audio_mix/dsp.rs`) rather than the
FFmpeg compressor first tried here, which allocated on the mixing thread. This
plan, by Augustas (pingvis, pull request #11), shaped the panel and its meters;
the rest is kept as he wrote it, and later processors below remain proposals.
Prepared on 20 October 2026 on `codex/audio-improvements`, based on fork main
`82c668d`. The initial prototype reuses the existing settings on one page with
compact/expanded controls. It remembers the view per target within the current
run; restart persistence is deferred until the layout is approved. Expand/restore
sits immediately left of the close button in a shared header. Expanded mode has
a left-hand General category and the existing settings on the right. Dynamics
now adds compression and pre/post FX meters; no EQ or other DSP. Keep this work separate
from the Windows GPU and Properties-layout PRs.

Scheduled layout pass: 21 mixer UI tests pass, including header-button ordering,
General-category placement, view memory, header dragging, close behavior and
the existing settings command flows. Debug build, production library/binary
Clippy and focused formatting/whitespace checks pass. The review app is opened
from `work/audio-layout-review/nida-audio-layout-v2.exe` in its own workspace
(API port 8102). Automated live visual verification was blocked by Computer Use
app approval timeout. The user subsequently reviewed and approved the layout.

Selected implementation: FFmpeg's embedded `acompressor` for the Dynamics category,
with separate stereo pre-processing and post-processing meters plus gain
reduction. These meter taps describe processor input/output, not pre/post fader;
the existing strip/output meters remain distinct. Compression stays disabled by
default. Packaged listening and other-platform validation remain release gates.

Initial evaluation was in `tools/audio-compressor-probe` (in pull request #11;
not kept in Nida, which does not use FFmpeg's compressor):
11 signal/bridge tests and repeatable release measurements of 1/8/16/32 stereo
strips. The embedded FFmpeg compressor is a viable candidate on the tested
Windows CPU. The following measurements predate production integration and do
not qualify the whole application. Direct f64 bridging
reduces typical service cost compared with automatic format conversions;
runtime makeup changes are abrupt and need host smoothing. No application
meters or DSP controls have been added by the probe.

## Product direction

Build a dependable live-production channel strip, not a miniature DAW. Start
with native processors, excellent metering and safe live edits. A compressor
alone is useful; a large plugin host is not a prerequisite.

vMix provides per-input audio settings, EQ, compression and gating, and supports
VST3 plugins on inputs and output buses. Use the familiar entry points without
assuming that another effect list is an improvement:
[audio settings](https://www.vmix.com/help29/AudioSettings.html),
[plugins](https://www.vmix.com/help29/AudioPlugins.html).

Our proposed differentiators are a clear signal-flow overview, visible gain
reduction and latency, genuinely useful voice presets, and predictable behavior
while recording, switching or streaming. These are design goals, not comparative
quality/performance claims.

## UI recommendation

Use **one modeless, resizable Audio Inspector**, initially a floating window.
Do not add an always-visible panel that reduces the space for video or faders.
Keep its contents independent of the window shell so a dockable inspector can
be added later without duplicating the editor or DSP logic.

### Entry points

- Reuse the existing **gear beside the fader** on each input and bus, including
  Master. It opens the compact inspector initially, remembering compact/expanded
  per target within the UI session. The expand/restore control sits on the right
  beside close, not in the settings body. Expanded mode has a left category list;
  the layout prototype implements only General with the existing controls.
  Merely opening it must never enable/bypass an effect. The headphones settings
  remain separate initially.
- Put General (gain/balance, devices, delay, routing and Audio Auto as applicable)
  and future processing categories inside that window. Do not add a separate FX button to the name
  header or consume another row beside the fader.
- Keep existing name interactions: double-clicking a clip name opens Input
  Settings; bus names retain rename/context actions. Do not place overlapping
  processing hit targets over the existing name header.
- Offer an Audio processing menu entry in the strip context menu as an alternate
  route. A later Input Settings Audio link should call the same open action.
- Do not put a global FX button in the top application toolbar: the processor
  belongs to a source or bus and its identity must be obvious.

The current strips are 104 logical pixels wide, with 22-pixel headers and
28-pixel buttons beside the fader. The user's mixer screenshot confirms that
long names are already clipped, so the earlier header-FX proposal is withdrawn.
Another vertical button would also steal fader travel and can overflow short
strips. Reusing the gear costs neither name width nor another control row.
Keep its accessible label and expand its tooltip to mention audio processing.

### Name readability and compact-state feedback

- Preserve the full header width for the name. Prefer intentional ellipsis
  over clipping text in the middle of characters; show the full name on hover
  and in the resizable inspector title/header.
- Consider a saved, short mixer alias ("Lectern", "Playback 1") independent
  of the filename/deck name, keyed by source UUID. This is a proposed addition,
  not an existing alias field; renaming an alias must not rename the media file.
- Consider a manual Compact/Comfortable strip-width preference after testing
  many-input layouts. Do not automatically reposition faders as sources appear
  during a live show merely to fill unused panel space.
- If processing needs an at-a-glance status, add a subtle marker within the
  existing gear's bounds and an explicit tooltip. It must not reduce the header
  or change hit targets. Do not reuse on-air red/green as an ambiguous effect
  state. The inspector itself shows enabled processors and chain bypass clearly.

### Inspector sketch

```text
Lectern mic — Audio                           [restore] [close]
[General]       | General settings
[EQ]            | Existing settings in the initial prototype
[Dynamics]      | Future processor controls after layout approval
[Meters]        |

Input  ||||||    Output  |||||    Reduction  3.2 dB
[Preset: Custom v]   [Copy] [Paste]       [Bypass chain]

Signal: Trim -> HPF/EQ -> Expander -> Compressor -> Fader

[on] Compressor                         [Reset]
     transfer curve             gain-reduction meter
     Threshold   Ratio   Attack   Release
     Knee        Makeup gain      Stereo link

[off] High-pass / EQ                     [expand]
[off] Expander                          [later release]
```

Show only implemented processors in the shipping UI; the sketch is a roadmap,
not a reason to ship nonfunctional controls. The current inspector has General
and Dynamics only. Other categories above are future design ideas.

- Clicking a different strip's gear changes the explicit target; selecting a video
  input or channel elsewhere does not silently retarget an open audio editor.
- Store target kind + UUID, never a strip index. If removed or replaced, close
  or disable the editor rather than applying its controls to the next strip.
- Put a real preset name and a Modified indication in the inspector. Defer A/B
  and level-matched audition until their behavior is tested; do not label an
  ordinary bypass "level matched".
- Opening, selecting tabs, moving the window and resizing are UI-local actions.
  Only parameter/bypass/preset changes issue engine commands. Keyboard focus
  must not accidentally fire transport shortcuts while typing a threshold.
- Distinguish "editor open" from "processing enabled" visually. Gear hover text
  can report active processors; do not use the same light for both states.

## Signal flow and compatibility

The current mixer is 48 kHz stereo in 480-frame (10 ms) blocks. Input PFL is
taken before input delay, fader, mute and Audio Auto. Bus monitoring is taken
after its fader but before output sync delay. Bus delivery currently clamps
samples to [-1, 1]; that is hard clipping, not a limiter. These behaviors are
visible in `src/internal/audio_mix/core.rs` and must be tested before changing.

Proposed source path:

```text
resampled source -> existing RAW PFL tap
                 -> input trim -> HPF/EQ -> expander -> compressor
                 -> optional PROCESSED audition tap
                 -> existing input sync delay
                 -> fader/balance/mute/Audio Auto -> meter -> bus sends
```

Process a source once before fan-out so sending it to several buses does not
advance the same detector multiple times. Fader moves must not change a
pre-fader compressor's detector level; introduce input trim separately from
the existing fader. Preserve raw PFL by default. A later processed-audition
switch changes headphones only, never the program or recording feed.

Proposed bus path:

```text
sum -> bus EQ/compressor -> bus fader/mute -> optional final limiter
    -> output meter -> monitor tap before sync delay
    -> existing bus sync delay -> devices / recordings / streams / DeckLink
```

The final limiter must follow any level boosts, and the monitor should hear
the processed bus before its existing picture-alignment delay. Apply bus DSP
once before delivery to consumers, not independently for every output.
The legacy direct-device recording passthrough bypasses this mixer: label that
explicitly; do not imply bus effects process a direct capture feed.

All new processors start disabled. Old stage files must produce the existing
samples and preserve routing, fades, monitoring and recording timing. The
identity of a source is not its display name. Reconnection, source replacement,
seek, loop and show load need explicit state-reset policies and tests.

## Engine foundation

Keep three layers separate:

1. **Configuration**: typed, serializable processor settings attached to
   `AudioStripConfig` and `AudioBusConfig`. Use additive defaults. Source strips
   deserialize through `StripOnDisk`; update that conversion too, not just the
   public struct. DSP histories, meter peaks and editor state are not serialized.
2. **DSP state**: native processors owned by `core::Source` / `core::Bus`, keyed
   by UUID, operating in place on stereo blocks. Preserve envelope/filter state
   across normal UI updates and `AudioEngine::apply`; don't recreate a rack
   whenever a fader or unrelated route changes.
3. **Control and telemetry**: engine commands validate settings; bounded meter
   snapshots expose input/output peaks, gain reduction and added latency.
   Neither the UI nor an HTTP handler processes samples.

Start with typed native settings and a documented fixed order, not an arbitrary
plugin graph, trait-object registry or persisted implementation-specific DSP
objects. Separate config/state allows a deliberate migration to an ordered rack
if a concrete reorder/plugin requirement appears later.

Suggested first processor: a stereo-linked, peak-detected soft-knee compressor
with threshold, ratio, attack, release, knee and manual makeup gain. A possible
voice preset is -18 dBFS / 3:1 / 10 ms attack / 150 ms release / 6 dB knee /
0 dB makeup; it is a starting point for listening tests, not a universal mic fix.
Stereo linking must preserve image position. Auto makeup, sidechain routing,
multiband and lookahead are deliberately deferred.

- Retain the existing mixing thread; no work in the render loop or CPAL callback.
- New steady-state DSP must allocate no buffers, acquire no extra locks and
  perform no logging, file IO, device creation or plugin scanning per block.
  Prepare storage/coefficients outside the critical mixing section. Publish
  bounded changes at block boundaries; smooth parameters per sample as needed.
- The current mixer already uses a mutex and has some block-time allocations.
  Measure them and lock hold time; do not claim the whole engine is lock-free
  or allocation-free. Avoid a complete threading rewrite without evidence.
- Preserve filter/envelope state; smooth gain and bypass changes to avoid clicks.
  Define reset behavior for actual discontinuities, not every control update.
- Reject invalid parameters consistently through UI/API/engine paths. Test
  NaN/Infinity, invalid time constants, extreme inputs, silence and denormals.
- Built-in lookahead-free compression is not a safety limiter and adds no
  intentional sample delay; existing buffering/device latency still exists.
- A future lookahead/true-peak limiter needs measured delay, output A/V handling
  and tests for parallel paths and bypass. Do not call a sample-peak meter or
  simple clamp "true peak". Keep the current clipping safeguard until a tested
  replacement is proven safe.

## Where to work

| Area | Existing integration point | Planned addition |
| --- | --- | --- |
| DSP | `src/internal/audio_mix/core.rs` | private `processing/` module; per-source/bus processor state |
| Persistence / config | `src/internal/audio_mix/mod.rs` | additive processing config and `StripOnDisk` migration |
| App commands | `src/app/state/audio_mix.rs`, engine command definitions/dispatch | typed source/bus processor edits and validation |
| API | `src/usecases/api/routes/audio_mix.rs` | processor-specific routes using the same commands |
| Snapshots | audio mix snapshots and UI snapshot mapping | processing settings and bounded meters |
| UI | `src/usecases/ui/panels/audio_mixer.rs` | expand existing gear/settings entry point into shared inspector shell |
| UI detail | proposed `src/usecases/ui/panels/audio_processing.rs` | reusable processor editor, no audio IO |
| Tests | `tests/audio_mix.rs`, existing core/UI tests | compatibility, DSP reference and target/lifecycle tests |
| Benchmark | new mixer/DSP benchmark | block deadlines and whole-mixer cost; not the existing beat-analysis benchmark |

Proposed API shape: processor settings beneath
`/api/audio-sources/{uuid}/processing/compressor` and
`/api/audio-buses/{uuid}/processing/compressor`, plus chain bypass. Names and
schemas are not implemented yet. Update only the named processor, not the whole
mixer snapshot. Document live edits' last-write-wins behavior before adding
remote control, and avoid undo/show-recall surprises in this existing live mixer.

## Small, reviewable delivery stages

### 0. Baseline and inspector foundation

- [ ] Record current mixer/monitor/routing tests and unprocessed sample fixtures.
- [ ] Measure baseline whole-mixer p50/p95/p99 block service time, CPU,
  allocations and missed 10 ms deadlines at 1/8/16/32 sources and several buses.
- [ ] Expand the gear's settings window into the shared inspector without changing audio.
  An honest empty state says "No processing enabled"; no fake meters/toggles.
- [ ] Test small strips, many sends, scaling, long names, UUID targeting, removed
  inputs/buses and existing rename/Input Settings interactions.

### 1. Compressor end to end

- [ ] Add disabled-by-default config, persistence, command validation and DSP.
- [ ] Add controls, transfer curve, input/output peak and gain-reduction meters.
- [ ] Test unity/disabled transparency, static gain curve, stereo linking,
  attack/release at several block sizes, bypass/parameter discontinuities,
  multiple bus sends, pause/resume, seek/loop and reconnect/reset behavior.
- [ ] Test old stage fixtures, round-trip saves, UI/API command parity and no
  changes to raw PFL or Audio Auto. Add offline speech/music listening fixtures
  with recorded provenance; don't normalize away compressor mistakes.
- [ ] Repeat baseline workload with identical build/fixtures/settings and no
  concurrent builds. Report processing cost and latency separately from CPU.

### 2. First complete native channel strip

- [ ] High-pass filter and four-band parametric EQ with response plot and
  coefficient-transition/stability tests.
- [ ] Expander with hold/hysteresis; voice presets reviewed on real speech.
- [ ] Bus safety limiter, with honest sample/true-peak distinction and explicit
  latency/A/V behavior. Do not ship lookahead without alignment validation.
- [ ] Useful presets and per-processor reset/copy/paste; no silent changes to
  old stages. Manual listening complements rather than replaces signal tests.

### 3. Differentiators after the baseline is solid

- [ ] LUFS / true-peak / loudness history, calibrated to reference fixtures.
- [ ] Speech-sidechain music ducking, de-esser, and opt-in noise suppression.
- [ ] Conference automixing and mix-minus, after defining independent send
  levels/pre-post taps and checking the current source-to-bus-only routing model.
- [ ] Audition A/B, optional docking and safe parameter-scene recall.
- [ ] Evaluate isolated VST3 hosting only if native coverage is insufficient;
  plugin compatibility, crashes and latency compensation are separate projects.

Every stage needs a named scope, disabled-path regression checks, measured
processing budget and a packaged Windows listening check. Other-platform and
device coverage should be explicit, not inferred from a headless test pass.
