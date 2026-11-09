# Audio tempo: Nida 0.9.7 reconciliation

Updated 1 November 2026 against upstream `45e6f0b`.

The old branch changed the capture constructor that v0.9.7 moved into its asynchronous device opener. Resolving by taking either side would discard either tempo analysis or the new device lifecycle.

The merge keeps upstream's device numbering, deferred open/close/reopen paths, silence/liveness tracking, feed handoff and PCM gathering. Tempo range is shared with native openers through an Arc captured by the manager's opener. Existing and future analysis workers observe range updates. The callback retains upstream's original raw PCM/live-bus tee, including its feeding gate, and sends only mono analysis hops to the bounded FFT/tempo worker. The worker no longer contains a second PCM-forwarding path.

Fresh validation:

- 102 existing audio and clock regressions pass, zero failed/ignored, in an isolated Windows harness importing the exact production audio and clock modules. It uses the unchanged production MIDI DeviceId alias (`u32`) as its only type-only shim. Tests cover rhythm/phase, sample rates, queue overload, worker/direct analysis equivalence, tempo range, silence, clock priority/manual control, live feeds, PCM gathering and asynchronous capture reopening/handoff. The harness does not emulate those implementations.
- Audio implementation, analysis module and benchmark pass focused rustfmt. Proposal whitespace checks pass.
- Compared rustfmt edits with PR-added/modified lines in every changed Rust file. Corrected the new tempo command's method-chain formatting; the remaining formatting findings in shared edited files concern inherited upstream lines.
- A fresh full production-library check reports exactly the two existing Windows DeckLink E0425 errors for Unix clock_gettime/CLOCK_THREAD_CPUTIME_ID at `src/internal/decklink/mod.rs:362`. No DeckLink guard or test suppression is included.
- This is not a full application, physical microphone, real-music accuracy, or native macOS/Linux qualification. Previous unrelated Linux suite/platform-lint failures still require separate assessment by fresh upstream CI. Other inherited whole-repository formatting findings are not hidden or reformatted wholesale.

Previously recorded estimator timings remain historical measurements; no performance numbers were rerun during this refresh.

