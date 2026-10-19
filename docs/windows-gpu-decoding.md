# Windows hardware decoding — local prototype

Windows GPU decoding remains experimental and off by default. The Setup toggle
opts in without changing macOS automatic hardware selection.

FFmpeg's D3D11VA decoder can now feed the same hardware-frame download and
colour-conversion path used by VideoToolbox on macOS. Windows activation is
experimental and opt-in: open **Layout → Panels → Setup**, then enable
**Performance → Use GPU video decoding (experimental)**. The choice is saved
in `Nida/performance.json` in the user's settings folder, across shows and
restarts. It applies to videos and streams opened afterward; already-open
players keep their decoder. Unsupported videos still fall back to CPU.

`VARDA_WINDOWS_HARDWARE_DECODE=1` supplies the initial choice when there is no
saved preference. A saved choice takes precedence, so the checkbox can disable
an environment-enabled test launch too. `VARDA_SOFTWARE_DECODE` overrides both
and forces software decoding; the UI explains why its checkbox is disabled.
macOS keeps its existing automatic hardware selection.

The decoder checks the codec's advertised hardware configurations before
creating a device. Unsupported codecs and unavailable devices use the regular
multithreaded software decoder. AV1 uses FFmpeg's native decoder only when a
hardware device was attached. Clip and stream callers retain the actual
selection, and existing mid-clip failure recovery reopens software decoding.
The input's playback information reports whether frames actually came from
the media engine.

All 82 video unit tests passed with both default startup and Windows opt-in. The
hardware picture comparison explicitly required and confirmed GPU decoding
for H.264, HEVC, AV1 and 10-bit HEVC on the RTX 3060 Ti. macOS was not run on
this Windows machine.

This step does not add zero-copy textures or change recording/stream encoding.
Downloaded NV12/P010 frames still go through CPU colour conversion and upload.

## Initial prototype measurements

Machine: NVIDIA GeForce RTX 3060 Ti, driver 32.0.16.1692. Synthetic 3840×2160,
30 fps H.264 clip, two seconds, produced with FFmpeg's `h264_nvenc`.

Criterion's `video_decode` benchmark measures seeking to the middle of the clip,
decoding forward and producing RGBA through Nida's `VideoPlayer`. In the dev
profile (native FFmpeg remains optimized), software took 106.60 ms and hardware
183.93 ms. Hardware seeking is slower in this case, so Windows stays opt-in.
The software case is an explicit CPU baseline using the same clip and wrapper;
this is not a before/after measurement of two Git revisions.

A separate FFmpeg continuous decode-to-RGBA run over all 60 frames used 5.281 s
of process CPU time in software and 1.890 s with D3D11VA, approximately 64% less.
Elapsed time was 0.310 s versus 0.485 s; maximum reported memory was roughly
528 MiB versus 1022 MiB. These are diagnostic CLI measurements, not application
FPS guarantees. Longer clips, several simultaneous clips, seeking, looping,
reverse playback and other GPU vendors need evaluation before enabling it by
default.

The first H.264 frame's decoded YUV samples matched byte for byte between CPU
and D3D11VA. FFmpeg's planar and semiplanar RGB converters produce different
edge interpolation, so picture-comparison tests repack the software reference
to the hardware frame's YUV packing before running the same RGBA conversion.
The existing strict image-difference threshold is retained.

To repeat the Criterion comparison, set `VARDA_VIDEO_BENCH_CLIP` to a clip and
run `cargo bench --bench video_decode` (use `--profile dev` for a quicker local
build). The benchmark refuses to label a software fallback as hardware.

## Nearby forward scrubbing

Short forward seeks (up to 0.25 seconds beyond the currently decoded picture)
now retain the decoder's reference pictures, queued frame and GPU surfaces.
They decode forward rather than flushing and starting again at a keyframe.
Backward and larger jumps, ping-pong, EOF and unavailable timestamps keep the
normal seek path. A decoder reopened in software always starts from a keyframe.
A repeated paused seek to the same picture delivers it once and then holds.

A separate Criterion baseline was recorded before this change. The same 4K
clip was scrubbed forward one frame at a time, with a backward reset every
eight steps. In the dev profile, mean estimates changed as follows:

| Decoder | Before | After |
| --- | ---: | ---: |
| Software | 99.52 ms | 18.05 ms |
| D3D11VA | 126.05 ms | 42.06 ms |

This is a scoped improvement to nearby forward scrubbing, not arbitrary seek
latency or an application FPS claim. CPU decoding still has lower latency in
this comparison; Windows hardware decoding remains opt-in. The benchmark's
coarse-seek case now alternates positions so it cannot accidentally measure a
repeated request for the same cached picture.

Four focused checks passed: forward/repeated seek and pause/resume correctness
on both decoder requests, keyframe recovery after hardware fallback, the
existing seek-to-frame pixel check, and forward-offset playback. Production
library/binary and benchmark Clippy passes. GPU changes remain local.
