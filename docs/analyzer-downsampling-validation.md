# Analyzer downsampling: upstream refresh

Updated 1 November 2026 against Nida v0.9.7 (`45e6f0b`).

The review branch now includes current upstream. The reduction pass and worker-request implementation remain outstanding upstream, so no adopted analyzer scope was removed. The two new benchmark expressions reported by rustfmt were corrected.

Fresh checks:

- All seven changed Rust files, including the benchmark, pass focused rustfmt.
- Proposal whitespace checks pass.
- `cargo check --locked --offline --lib --no-default-features --features screen-capture` reaches the application but fails with exactly two E0425 diagnostics in unchanged `src/internal/decklink/mod.rs:362`: `libc::clock_gettime` and `libc::CLOCK_THREAD_CPUTIME_ID` do not exist on Windows. No temporary portability patch was included or used to claim a passing check.

Earlier exact-feature GPU/readback and worker regressions and performance measurements remain historical evidence in the PR; they were not rerun by this formatting/merge refresh. A fresh full application test pass is not claimed.

Previous upstream Actions failures also include Linux platform-stub lints, systemd inhibitor/DeckLink/enum-size tests, missing FFmpeg fixture tools, and Mac-only presentation assumptions. The separate CI-tools PR #9 supplies the executables; it is not included here. A fresh CI run on the updated head is needed before qualifying this branch on Linux/macOS.

References:

- [Previous analyzer test job](https://github.com/kristijonasatpro/Nida/actions/runs/41225176404/job/124607716756)
- [Previous Windows compilation blocker](https://github.com/kristijonasatpro/Nida/actions/runs/41225176460/job/124607717307)
- [Fixed benchmark formatting diagnostics](https://github.com/kristijonasatpro/Nida/actions/runs/41225176405/job/124607717496)

