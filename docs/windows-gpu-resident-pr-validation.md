# Isolated upstream GPU port: validation status

Prepared against upstream `6f1b4d7`, retaining the history of existing PR #6.
This is one experimental Windows video-decoding/pacing feature. Other fork
backends, UI-responsiveness work, packaging, recording changes and broad test
lint cleanup are excluded. Delivery diagnostic values live in the engine layer.
Retained forward seeks and new clock policies remain Windows-only; non-Windows
periodic waits and play-command clock behavior keep their inherited policy.

The maintainer has [deferred Windows support](https://github.com/kristijonasatpro/Nida/pull/1#issuecomment-6297870074)
because no Windows test machine is available. The existing PR is a draft/reference
update, not a request to bypass that policy or a claim that upstream CI passed.

## Fresh local checks of the isolated port

| Check | Result |
| --- | --- |
| CPU/WGSL/metadata/ownership harness | 28 passed, zero failed, one existing timing probe ignored |
| Harness all-target Clippy, warnings denied | Passed |
| Harness formatting | Passed |
| New Rust module formatting / whitespace | Passed |
| Full application production type check on Windows | Blocked by existing DeckLink `libc::clock_gettime` and `CLOCK_THREAD_CPUTIME_ID` calls |
| Full application all-target Clippy on Windows | Blocked by the same two existing DeckLink errors |
| Untouched upstream production type check | The same two DeckLink errors |
| Full repository formatting | Fails: 1,853 differences in 173 files; untouched upstream has 1,856 in 174 files, with no newly failing file introduced here |
| Full library / UI integration execution | Not completed; prerequisite standalone Windows core build is blocked |

An attempted library test build also re-enabled the inherited default feature
set through the self dev-dependency and began compiling Servo; that attempt was
stopped. The separate Windows portability PR #1 handles the core build/feature
selection. No baseline error or test gate is suppressed here. Platform CI and
standalone application/hardware qualification are still required before this
upstream port is considered ready for integration.

Commands, after configuring normal MSVC/FFmpeg 9/libclang prerequisites:

```powershell
cargo test --offline --manifest-path tools/gpu-video-checks/Cargo.toml
cargo clippy --offline --manifest-path tools/gpu-video-checks/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path tools/gpu-video-checks/Cargo.toml -- --check
cargo check --offline --no-default-features --features screen-capture --lib -j 1
cargo clippy --offline --all-targets --no-default-features --features screen-capture -j 1 -- -D warnings
cargo fmt --all -- --check
```

## Integration qualification is separate

The Windows integration fork's release binaries passed actual app pacing,
36 seek-phase windows, colour/seek/pool checks and a one-hour seven-fixture soak.
Those measurements and their limits are linked from
[qualification provenance](windows-gpu-resident-qualification.md). They are
evidence for the implementation tested there, not fresh standalone upstream
binary results. The P010 shader is the corrected production shader; defaults,
colour tolerances, zero-picture-transfer checks and three-slot bounds are intact.

The preference remains opt-in in per-user `Nida/performance.json`. No `.varda`
scene/stage format changes are introduced. Playback API snapshots gain additive
residency/delivery fields. AMD/Intel and physical scanout qualification are deferred.
