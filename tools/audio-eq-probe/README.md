# Production EQ signal and budget probe

This small workspace imports the production EQ, compressor and reader modules directly;
it is not a second implementation. FFmpeg filters must initialize successfully
for signal/performance tests to pass. Use the same FFmpeg SDK/library search
path as the app.

```powershell
. 'E:/Custom Apps/Nida/work/env.ps1'
$env:CARGO_TARGET_DIR='E:/Custom Apps/Nida/target'
$env:CARGO_INCREMENTAL='0'
cargo test --offline --locked --manifest-path tools/audio-eq-probe/Cargo.toml
cargo test --offline --locked --release --manifest-path tools/audio-eq-probe/Cargo.toml --no-run
$binary=(Get-ChildItem "$env:CARGO_TARGET_DIR/release/deps/audio_eq_probe-*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
./scripts/measure-audio-eq.ps1 -Binary $binary -Test equalizer_bank_budget_probe -Mode disabled -OutputRoot ./measurements/eq-release-off
./scripts/measure-audio-eq.ps1 -Binary $binary -Test equalizer_bank_budget_probe -Mode sources -OutputRoot ./measurements/eq-release-on
```

Repeat three times serially without concurrent builds. The allocator counter
only covers Rust allocations in the timed steady-state loop, not FFmpeg's C
allocations or graph setup. Readiness assertions reject silent dry fallback.
This probe excludes the mixer, renderer, devices and playback scheduling. It
does not measure live-app CPU percentages or prove cross-platform performance.

[Design](../../docs/audio-equalizer.md),
[measurements and limitations](../../docs/audio-equalizer-measurements.md).
`results/windows-2026-10-20.json` preserves the before/after production-core
and standalone release-probe rows, process metadata and binary hashes.

The signal harness also runs the production compressor regressions; the ignored
release benchmark itself still measures EQ only. For full mix-core EQ plus
compression, use the app's test binary with `measure-audio-eq.ps1 -Compressor`
as documented in the measurements. Its separate results are preserved in
`results/windows-compressor-eq-2026-10-20.json`. Neither measurement counts
FFmpeg C allocations or substitutes for native live-load tests.
