# Capture performance validation

Use an optimized release build when measuring interactive capture. The default
development build is unoptimized; its CPU resize throughput is not a release
performance baseline.

## CPU resize benchmark

The Criterion target opens no display or capture session:

```sh
cargo bench --locked --no-default-features --features screen-capture --bench capture_resample
```

It covers 2560x1080 to 1920x810 and 3840x2160 to 1920x1080 box filtering.
For a before/after comparison, first put the same benchmark file and Cargo
target registration in the baseline checkout without the filter changes. Keep
the Criterion results directory between these runs:

```sh
# Original filter with the same benchmark harness:
cargo bench --locked --no-default-features --features screen-capture --bench capture_resample -- --save-baseline before
# Optimized filter:
cargo bench --locked --no-default-features --features screen-capture --bench capture_resample -- --baseline before
```

Close other CPU-heavy applications and keep the build profile and toolchain
identical. The filter is shared by Windows, Linux X11 and Linux Wayland capture;
macOS scales in ScreenCaptureKit.

## Windows observations (2026-10-17)

On a Ryzen 9 5950X, same-source optimized filter microbenchmarks measured median
times of 10.05 to 5.93 ms for the ultrawide reduction and 17.65 to 11.40 ms for
4K to 1080p. Both filters produced identical bytes for the large fixtures and
34,224 small geometry comparisons. Criterion runs also showed lower filter
time, but overlapped application compilation and had substantial variability.

The Windows deadline change avoids resetting the capture phase on each
accepted callback. A bounded early tolerance accommodates compositor jitter;
stalls skip missed deadlines rather than causing a catch-up burst. A cached
D3D11 staging texture is reused until the cropped extent changes.

With an RTX 3060 Ti, a 2560x1080 primary display, and a 1920x1080 engine canvas,
the combined Windows build delivered approximately 30 fresh capture frames per
second at 30 requested and 55 at 60 requested. The later filter/readback
optimizations reduced CPU work without materially changing these rates. They
do not establish full 60 FPS delivery or faster recording/stream encoding.

These Windows application measurements include the separate core portability
fixes. The capture changes alone do not resolve unrelated Windows build issues
in the upstream baseline. Linux runtime performance was not measured.

When comparing colors, record the display's active HDR/Advanced Color mode and
compare known swatches on the same display through raw capture and final PGM
presentation. Exact raw capture pixels on one SDR display do not establish
accuracy on another display or through the entire output pipeline. A reported
washed-out secondary-display output remains unresolved.
