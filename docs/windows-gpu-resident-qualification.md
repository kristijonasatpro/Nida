# Windows GPU-resident video: qualification provenance

This upstream feature port is based on current upstream main and the existing
Windows GPU-decoding PR. It excludes unrelated Windows backends and the fork's
broad test-lint cleanup. Its source is adapted for upstream's engine value layer;
it is not the exact binary used for the following hardware measurements.

## Measured Windows integration

The integration fork's frozen release harness and production app at c19e528
passed all pacing acceptance checks on RTX 3060 Ti / Ryzen 9 5950X, Windows DX12,
FFmpeg 9, driver 621.24. The shader override was unset.

- Actual app 1080p60, 4K60, GUI pause/seek and fractional-seek hold: approximately
  60 distinct pictures/s over measured 60-second intervals, zero empty takes or
  replacements. Fast playback: 60 distinct pictures/s over 20 seconds at 2x,
  retaining the existing output cap.
- All 36 short seek-phase windows pass, with 899 initial misses recovered and
  no residual loss. Three-clip playback exercises 5,505 successful waits; maximum
  observed shared-window span is 1.3854 ms. These are finite timing observations,
  not hard Windows deadlines.
- Paired copy-readiness p95: 1.641 to 0.805 ms at 1080p; 2.861 to 1.461 ms at 4K.
  These CPU observations do not establish GPU timestamp service time.
- 28 CPU checks, 194 video regressions, six fixture groups/30 picture comparisons,
  interlaced fallback, native event lifecycle and three-minute playlist soak pass.
- A later one-hour soak on that same frozen harness passes: 216,002 pictures,
  239 seeks, 29 reloads, seven fixtures including 4K60; peak RSS 503,422,976 bytes.
  This is a stability harness, not a one-hour actual-app PTS/scanout benchmark.

Earlier two-hour soaks passed on RTX 3060 Ti and RTX 2060 Max-Q before the pacing
follow-up. SDR P010 was corrected after the Max-Q exposed a conversion error.
The earlier focused 3060 Ti P010 recheck used an existing harness plus an explicit
shader override; do not confuse it with the later freshly built production app.
The new pacing change has not been separately qualified on AMD/Intel or Max-Q.
No general CPU improvement is claimed. GUI observations were API driven and
obscured; physical output and audio synchronization remain unqualified.

Permanent integration evidence and reproduction manifests:

- [Pacing qualification](https://github.com/pingvis/Nida-Windows/blob/c19e528/docs/qualification/rtx3060ti-pacing-fence-event-20261120/README.md)
- [One-hour soak](https://github.com/pingvis/Nida-Windows/blob/35f3e0f/docs/qualification/rtx3060ti-pacing-soak-1h-20261120/README.md)
- [Earlier Max-Q qualification](https://github.com/pingvis/Nida-Windows/blob/a5b98f1/docs/windows-gpu-resident-qualification-rtx2060-maxq.md)

## Isolated upstream port

CPU checks pass on this branch. Standalone production/test/format status is
recorded in the PR description, with current baseline failures called out rather
than inherited integration results presented as new upstream results. Keep the
PR draft until its upstream dependencies and full platform CI are validated.
Experimental status, default-off behavior, zero-transfer checks, colour tolerances
and three-slot lifetime safeguards remain intact.
