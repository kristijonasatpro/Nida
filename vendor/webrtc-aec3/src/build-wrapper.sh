#!/bin/bash
# Builds the C API (kk_aec), the CLI tool (aec_wav) and the embeddable static libs
# against the webrtc-audio-processing 2.1 install in ../install. arm64, macOS 13.0+.
set -euo pipefail
AEC="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$AEC/src/webrtc-audio-processing"
# kk_aec.cc uses one internal header (aec3/echo_canceller3.h), so it is compiled against the
# source tree with exactly the defines the library was built with (from compile_commands.json).
INC="-I$SRC/webrtc -I$SRC/subprojects/abseil-cpp-20240722.0"
DEFS="-DWEBRTC_LIBRARY_IMPL -DWEBRTC_ENABLE_SYMBOL_EXPORT -D_WINSOCKAPI_ -DNDEBUG -DWEBRTC_POSIX -DWEBRTC_MAC -DWEBRTC_ARCH_ARM64 -DWEBRTC_HAS_NEON -DWEBRTC_APM_DEBUG_DUMP=0 -D_LIBCPP_HARDENING_MODE=_LIBCPP_HARDENING_MODE_FAST"
CXXFLAGS="-std=c++17 -O2 -arch arm64 -mmacosx-version-min=13.0 -Wall"
WAP="$AEC/install/lib/libwebrtc-audio-processing-2.a"
mkdir -p "$AEC/bin" "$AEC/lib" "$AEC/build/tool"
cd "$AEC/build/tool"

# 1. C API object + thin static lib (needs libwebrtc-audio-processing-2.a at link time)
clang++ $CXXFLAGS $INC $DEFS -c "$AEC/tool/kk_aec.cc" -o kk_aec.o
rm -f "$AEC/lib/libkk_aec.a" && libtool -static -o "$AEC/lib/libkk_aec.a" kk_aec.o

# 2. One self-contained relocatable object: kk_aec + all of WebRTC/abseil, with every
#    symbol except _kk_aec_* made private (no clashes with sherpa-onnx/ONNX Runtime/abseil).
cat > kk_aec_public.exp <<'EOF'
_kk_aec_config_default
_kk_aec_create
_kk_aec_destroy
_kk_aec_render_frame_samples
_kk_aec_capture_frame_samples
_kk_aec_output_frame_samples
_kk_aec_process_render
_kk_aec_process_capture
_kk_aec_linear_output
_kk_aec_get_stats
_kk_aec_reset
_kk_aec_version
EOF
ld -r -arch arm64 \
   -exported_symbols_list kk_aec_public.exp \
   kk_aec.o -force_load "$WAP" -o kk_aec_prelinked.o
rm -f "$AEC/lib/libkk_aec_all.a" && libtool -static -o "$AEC/lib/libkk_aec_all.a" kk_aec_prelinked.o

# 3. CLI tool, linked the way the app would link: libkk_aec_all.a + libc++ + CoreFoundation
clang++ $CXXFLAGS -I"$AEC/tool" -c "$AEC/tool/aec_wav.cc" -o aec_wav.o
clang++ -arch arm64 -mmacosx-version-min=13.0 aec_wav.o "$AEC/lib/libkk_aec_all.a" \
   -framework CoreFoundation -Wl,-dead_strip -o "$AEC/bin/aec_wav"
# same tool linked against the plain archives (thin lib + upstream lib), as a cross-check
clang++ -arch arm64 -mmacosx-version-min=13.0 aec_wav.o "$AEC/lib/libkk_aec.a" "$WAP" \
   -framework CoreFoundation -Wl,-dead_strip -o "$AEC/bin/aec_wav_thin"
echo "built: $AEC/bin/aec_wav  $AEC/bin/aec_wav_thin  $AEC/lib/libkk_aec.a  $AEC/lib/libkk_aec_all.a"
