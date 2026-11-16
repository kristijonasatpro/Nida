# WebRTC echo canceller (AEC3)

The echo removal on an audio bus (`src/internal/audio_mix/echo.rs`) uses WebRTC's AEC3, the echo canceller in Chrome
and most call apps.

`lib/libkk_aec_all.a` (arm64, macOS 13.0 or later) is AEC3 from webrtc-audio-processing **v2.1** (commit
846fe90a289f58b7c9303a635142aa2c7caa93e5, freedesktop/PulseAudio) with abseil-cpp 20240722.0. It is linked into one
object behind the small C API in `include/kk_aec.h` (`src/kk_aec.cc`). Only the `kk_aec_*` symbols are exported, so it
cannot clash with any other copy of abseil or WebRTC in the app. Link it with `-lc++ -framework CoreFoundation`.

It was built for Kmynas klauso (the same author's call transcriber) and copied from its repository, tag v0.9.10
(sha256 of the library e5acca0a19bfc97128f903672a77dc3c733652d4ebd863cc4b51dbe96d1936aa).

## Licences

These go with the app, in `Nida.app/Contents/Resources/licenses`:

- WebRTC: BSD-3-Clause (`licenses/webrtc-LICENSE.txt`, `licenses/webrtc-COPYING.txt`) and its patent grant
  (`licenses/webrtc-PATENTS.txt`);
- abseil: Apache-2.0 (`licenses/abseil-LICENSE.txt`; abseil ships no NOTICE file);
- code inside WebRTC under licences of its own, each built into the library: Takuya Ooura's FFT
  (`licenses/webrtc-ooura-fft-LICENSE.txt`), Wilco Dijkstra's square root, public domain
  (`licenses/webrtc-spl_sqrt_floor-LICENSE.txt`), Mark Olesen's FFT (`licenses/webrtc-fft-LICENSE.txt`), PFFFT by
  Julien Pommier on UCAR's FFTPACK (`licenses/webrtc-pffft-LICENSE.txt`), and RNNoise (`licenses/webrtc-rnnoise-COPYING.txt`,
  with the Octasic notice from its `rnn_activations.h`).

## Source

The exact source the library was built from is kept in `upstream/`:

| File | What it is | sha256 |
|---|---|---|
| `webrtc-audio-processing-2.1.tar.gz` | webrtc-audio-processing at v2.1 (commit 846fe90a…), from `git archive` | `655ce719a9fa386df21ed9a9de02493f2157ede84100ed26c06c95d65631be60` |
| `abseil-cpp-20240722.0.tar.gz` | abseil-cpp 20240722.0, as meson's wrap downloaded it | `f50e5ac311a81382da7fa75b97310e4b9006474f9560ac46f54a9967f07d4ae3` |
| `abseil-cpp_20240722.0-3_patch.zip` | the meson build files for abseil (wrapdb 20240722.0-3) | `12dd8df1488a314c53e3751abd2750cf233b830651d168b6a9f15e7d0cf71f7b` |

The two abseil files have the hashes webrtc-audio-processing's `subprojects/abseil-cpp.wrap` asks for.

## Rebuilding

It takes about 30 s, using meson in a local venv and the Mac's own ninja and clang.

1. In an empty folder, put `src/build_all.sh`, and `src/build-wrapper.sh` as `tool/build.sh` with `kk_aec.cc` and
   `include/kk_aec.h` beside it.
2. To build from the copies in `upstream/` rather than the network: unpack `webrtc-audio-processing-2.1.tar.gz` as
   `src/webrtc-audio-processing`, put the two abseil files in its `subprojects/packagecache/`, and leave out
   `build_all.sh`'s `git clone` and `checkout` lines.
3. Run `build_all.sh`. Its last step (3., the `aec_wav` command-line tool) needs a source file that is not kept here:
   leave those lines out.

Keep `-Dneon=enabled`: without it NEON is never turned on and the canceller runs about 4 times slower.
