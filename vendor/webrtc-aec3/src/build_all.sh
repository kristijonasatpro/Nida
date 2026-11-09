#!/bin/bash
# From-scratch build of WebRTC AEC3 (webrtc-audio-processing v2.1) as static arm64 libs,
# plus the kk_aec C API and the aec_wav tool. Nothing is installed outside this folder:
# meson lives in ./venv; ninja/cmake/clang are the ones already on the Mac.
set -euo pipefail
AEC="$(cd "$(dirname "$0")" && pwd)"
cd "$AEC"

python3 -m venv venv
venv/bin/python -m pip install --quiet meson            # tests also use: numpy scipy

git clone --quiet https://gitlab.freedesktop.org/pulseaudio/webrtc-audio-processing.git src/webrtc-audio-processing
git -C src/webrtc-audio-processing checkout --quiet v2.1  # = 846fe90a289f58b7c9303a635142aa2c7caa93e5

export MACOSX_DEPLOYMENT_TARGET=13.0
# -Dneon=enabled matters: the default 'auto' never defines WEBRTC_HAS_NEON (about 4x slower).
# --force-fallback-for=abseil-cpp builds abseil 20240722.0 from the wrap and folds it into the .a
venv/bin/meson setup build/wap src/webrtc-audio-processing \
  --prefix="$AEC/install" --buildtype=release --default-library=static \
  --force-fallback-for=abseil-cpp -Dneon=enabled \
  -Dc_args=-mmacosx-version-min=13.0 -Dcpp_args=-mmacosx-version-min=13.0 \
  -Dc_link_args=-mmacosx-version-min=13.0 -Dcpp_link_args=-mmacosx-version-min=13.0
venv/bin/meson compile -C build/wap
venv/bin/meson install -C build/wap

"$AEC/tool/build.sh"
