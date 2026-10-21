#!/usr/bin/env bash
# Build Varda Linux portable tarball
# Usage: ./scripts/ci/build-linux.sh [--skip-deps] [--skip-build]
#
# Produces: Varda-Linux-x86_64.tar.gz in the project root
#
# Portable layout — extract anywhere, run ./varda:
#   Varda-Linux-x86_64/
#   ├── varda              (launcher script — sets LD_LIBRARY_PATH, execs bin/varda)
#   ├── bin/varda          (the actual binary)
#   ├── lib/               (bundled FFmpeg, codec, SRT shared libs)
#   ├── shaders/
#   ├── LICENSE
#   └── FFMPEG-LICENSE
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

SKIP_DEPS=false
SKIP_BUILD=false
for arg in "$@"; do
  case "$arg" in
    --skip-deps)  SKIP_DEPS=true ;;
    --skip-build) SKIP_BUILD=true ;;
    *) echo "Unknown arg: $arg"; exit 1 ;;
  esac
done

echo "==> Project root: $PROJECT_ROOT"

# --- Install system dependencies ---
if [ "$SKIP_DEPS" = false ]; then
  echo "==> Installing system dependencies..."
  sudo apt-get update
  sudo apt-get install -y \
    build-essential cmake pkg-config python3 \
    libvulkan-dev \
    libsrt-gnutls-dev \
    libasound2-dev \
    libv4l-dev \
    libfreenect-dev \
    libpipewire-0.3-dev \
    libwayland-dev libxkbcommon-dev libx11-dev libxrandr-dev libxi-dev \
    libgtk-3-dev
fi

# Match CI's commands, headers and shared libraries instead of Ubuntu's FFmpeg 6.
FFMPEG_PREFIX="${FFMPEG_DIR:-${FFMPEG_PREFIX:-$HOME/.local/nida-ffmpeg-9}}"
if [ "$SKIP_DEPS" = false ]; then
  python3 scripts/ci/setup-linux-ffmpeg.py --prefix "$FFMPEG_PREFIX"
  source "$FFMPEG_PREFIX/nida-env.sh"
fi
pkg-config --atleast-version=63 libavcodec \
  || { echo "FFmpeg 9+ is required (including with --skip-deps)" >&2; exit 1; }

# --- Build release binary ---
if [ "$SKIP_BUILD" = false ]; then
  echo "==> Building release binary..."
  cargo build --release
fi

STAGE="Varda-Linux-x86_64"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/lib" "$STAGE/shaders"

echo "==> Preparing portable directory..."

# Binary
cp target/release/varda "$STAGE/bin/varda"
cp "$(command -v ffmpeg)" "$(command -v ffprobe)" "$STAGE/bin/"

# Shaders
cp -r shaders/* "$STAGE/shaders/"

# Launcher script
cat > "$STAGE/varda" << 'WRAPPER_EOF'
#!/bin/bash
# Varda launcher — resolves bundled libs relative to this script
SELF=$(readlink -f "$0")
HERE=$(dirname "$SELF")
export LD_LIBRARY_PATH="${HERE}/lib:${LD_LIBRARY_PATH:-}"
export PATH="${HERE}/bin:${PATH}"
exec "${HERE}/bin/varda" "$@"
WRAPPER_EOF
chmod +x "$STAGE/varda"

# --- Bundle media shared libs (allowlist) ---
# Only bundle the media libs we need. System libs (libc, X11, Vulkan, GTK,
# ALSA, etc.) are left to the host.
MEDIA_LIBS="
  libavcodec libavformat libavutil libswscale libswresample libavdevice libavfilter libpostproc
  libsrt libsrt-gnutls
  libx264 libx265 libvpx libopus libvorbis libvorbisenc libogg libmp3lame libfdk-aac
  libass libharfbuzz libfribidi
  libaom libdav1d librav1e libSvtAv1Enc
  libtheoraenc libtheoradec libtheora
  libwebp libwebpmux
  libnuma libgnutls libhogweed libnettle libgmp
  libsoxr libvidstab libzimg librubberband libsamplerate
  libspeex libshine libtwolame libgsm
  liblzma libbz2 libsnappy
  libva libva-drm libva-x11 libva-wayland
  libOpenCL
  libfreenect libusb-1.0
"

echo "==> Bundling media shared libraries..."
for lib_base in $MEDIA_LIBS; do
  case "$lib_base" in
    libav*|libswscale|libswresample|libpostproc) media_dir="$(pkg-config --variable=libdir libavcodec)" ;;
    *) media_dir=/usr/lib ;;
  esac
  find "$media_dir" -name "${lib_base}.so*" -print0 2>/dev/null | while IFS= read -r -d '' lib; do
    libname=$(basename "$lib")
    if [ ! -e "$STAGE/lib/$libname" ]; then
      echo "  bundling: $libname"
      cp -P "$lib" "$STAGE/lib/" 2>/dev/null || true
    fi
  done
done

# --- Bundle NDI SDK (optional, runtime-loaded via libloading) ---
NDI_SDK_DIR=""
for d in "/tmp/NDI SDK for Linux" "$HOME/NDI SDK for Linux"; do
  if [ -d "$d" ]; then
    NDI_SDK_DIR="$d"
    break
  fi
done
if [ -n "$NDI_SDK_DIR" ]; then
  echo "==> Bundling NDI SDK from: $NDI_SDK_DIR"
  cp -P "$NDI_SDK_DIR/lib/x86_64-linux-gnu/"libndi.so* "$STAGE/lib/" 2>/dev/null || true
else
  echo "==> NDI SDK not found, skipping bundle"
fi

# --- Licenses ---
cp LICENSE "$STAGE/LICENSE" 2>/dev/null || echo "MIT License" > "$STAGE/LICENSE"
# The selected SDK enables GPL codecs; ship its actual licence and provenance.
cp "$FFMPEG_PREFIX/LICENSE.txt" "$STAGE/FFMPEG-LICENSE"
cp "$FFMPEG_PREFIX/nida-build-inputs.json" "$STAGE/FFMPEG-build-inputs.json"

# --- Create tarball ---
echo "==> Creating tarball..."
tar czf "Varda-Linux-x86_64.tar.gz" "$STAGE"
rm -rf "$STAGE"

echo "==> Done: Varda-Linux-x86_64.tar.gz"
ls -lh "Varda-Linux-x86_64.tar.gz"
echo ""
echo "Extract and run:"
echo "  tar xzf Varda-Linux-x86_64.tar.gz"
echo "  cd Varda-Linux-x86_64"
echo "  ./varda"
