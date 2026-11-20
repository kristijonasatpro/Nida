#!/usr/bin/env bash
# Build Nida.app for Apple silicon and the DMG to give it away in.
#
# Unlike scripts/make-local-app.sh (this computer only: FFmpeg used from
# where Homebrew put it), this app carries what it needs: the FFmpeg
# libraries Nida links, the `ffmpeg` program it runs for REC and STREAM, and
# every library those load (bundle-macos-libs.py), and the shaders. It opens
# on any Mac with Apple silicon and macOS 15.8.1 or later — Homebrew's
# libraries need the macOS they were built for, so build this on macOS 15
# (as the release workflow does). Most of Homebrew's packages for macOS 15
# need 15.0; tesseract's (FFmpeg's OCR filter, linked by ffmpeg-full) needs
# 15.8.1 since its October 2026 bottle, so that is the floor. Anything needing more stops the build.
#
# The FFmpeg inside is Homebrew's `ffmpeg-full`, under GPL version 3 or later,
# so the app as a whole is given away under the GPLv3; Nida's own source
# stays MIT. Resources/licenses has every bundled library's licence and where
# its source is. The NDI runtime is not bundled — its licence and the GPL's do
# not mix — so NDI works once the NDI runtime is installed (docs/23).
#
# Signed ad hoc, not by Apple: the first time, macOS needs Open Anyway in
# System Settings → Privacy & Security. With NIDA_SIGN_IDENTITY set to a
# code-signing certificate in the keychain (the mini's "Nida Local
# Signing"), signed by it instead: every build signed alike, macOS keeps the
# camera, microphone and screen permissions from one to the next.
#
# The GPL asks for the corresponding source to be offered beside the DMG:
# Nida-<version>-third-party-source.tar.gz holds the exact source Homebrew built
# each bundled library from, with its formula (see bundle-macos-libs.py).
# Shaders whose licence is non-commercial or not stated are left out
# (shaders-left-out.txt).
#
# Usage: scripts/ci/build-macos.sh [--skip-deps] [--binary PATH] [--out DIR]
#          [--min-macos 15.8.1] [--no-sources] [--label NAME] [--build-only]
#   --binary      package a nida already built, instead of building one
#   --build-only  build nida as the app carries it, say where it is, and
#                 stop — no app, no DMG. For --binary on another Mac, the
#                 one that packages and signs, spared the build: that Mac's
#                 Homebrew libraries go into the app, and it is for it to
#                 check that they are ones the program can run with (the
#                 app's own start, further down, is a smoke check, no proof)
#   --no-sources  skip the source archive (a try-out, not for giving away)
#   --label NAME  a try-out's name after the version — its bundle version
#                 and the DMG's name (Nida-0.9.1-ppt-70d687d-macOS-arm64.dmg)
# Produces in DIR (the project root by default): Nida-<version>-macOS-arm64.dmg,
# its .sha256, and Nida-<version>-third-party-source.tar.gz — nothing with
# --build-only
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

SKIP_DEPS=false
SOURCES=true
LABEL=""
BINARY=""
BUILD_ONLY=false
OUT="$ROOT"
MIN_MACOS="15.8.1"
BUNDLE_ID="lt.proconf.nida"
FEATURES="--no-default-features --features screen-capture"
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-deps) SKIP_DEPS=true ;;
    --binary)
      BINARY="${2:-}"
      [ -n "$BINARY" ] || { echo "--binary needs the path of a nida already built" >&2; exit 1; }
      shift ;;
    --build-only) BUILD_ONLY=true ;;
    --out) shift; OUT="$1" ;;
    --min-macos) shift; MIN_MACOS="$1" ;;
    --no-sources) SOURCES=false ;;
    --label) shift; LABEL="$1" ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
  shift
done

if [ "$BUILD_ONLY" = true ] && [ -n "$BINARY" ]; then
  echo "--build-only builds a nida: not with --binary" >&2
  exit 1
fi
if [ -n "$BINARY" ] && { [ ! -f "$BINARY" ] || [ ! -x "$BINARY" ]; }; then
  echo "--binary $BINARY: no program there" >&2
  exit 1
fi
if [ "$(uname -m)" != "arm64" ]; then
  echo "Build this on a Mac with Apple silicon" >&2
  exit 1
fi
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUILD="$VERSION${LABEL:+-$LABEL}"
echo "==> Nida $BUILD for macOS $MIN_MACOS and later"

if [ "$SKIP_DEPS" = false ]; then
  brew install ffmpeg-full
  # What the machine had installed before may be older than its formula,
  # or built there for its own macOS: every library the app will carry,
  # Homebrew's own package of today's version (bundle-macos-libs.py
  # refuses one that was not poured from a package).
  DEPS="$(brew deps --installed ffmpeg-full)"
  # shellcheck disable=SC2086
  brew unpin ffmpeg-full $DEPS 2>/dev/null || true
  export HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK=1
  # shellcheck disable=SC2086
  brew upgrade --force-bottle ffmpeg-full $DEPS || true
  # Still older than Homebrew's package: removed and poured afresh.
  # shellcheck disable=SC2086
  for formula in $(brew outdated --formula --quiet ffmpeg-full $DEPS); do
    brew uninstall --ignore-dependencies --force "$formula"
    brew install --force-bottle "$formula"
  done
  # shellcheck disable=SC2086
  brew reinstall --force-bottle ffmpeg-full $DEPS
  # shellcheck disable=SC2086
  echo "Homebrew packages the app will carry: $(brew list --versions ffmpeg-full $DEPS | tr '\n' ';')"
fi
BREW="$(brew --prefix)"
FFMPEG_FULL="$BREW/opt/ffmpeg-full"
[ -x "$FFMPEG_FULL/bin/ffmpeg" ] || { echo "No ffmpeg-full: brew install ffmpeg-full" >&2; exit 1; }
PKG_CONFIG_PATH="$FFMPEG_FULL/lib/pkgconfig" pkg-config --atleast-version=63 libavcodec \
  || { echo "FFmpeg 9+ development libraries are required; upgrade ffmpeg-full" >&2; exit 1; }


if [ -z "$BINARY" ]; then
  # What the environment would have cargo build otherwise than is said here
  # — other flags over these, another target or profile, another FFmpeg than
  # ffmpeg-full: refused, not overruled (PKG_CONFIG_PATH is set below). Not
  # all that could change it is looked for: another compiler, a wrapper, or
  # flags for the C parts (RUSTC_WRAPPER, CC, CFLAGS) pass.
  CHANGES_BUILD='^(CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_TARGET|CARGO_PROFILE_RELEASE_.*|FFMPEG_DIR|BINDGEN_EXTRA_CLANG_ARGS(_.*)?|(HOST_|TARGET_)?PKG_CONFIG(_.*)?)$'
  names=0
  while IFS= read -r -d '' pair; do
    name="${pair%%=*}"
    names=$((names + 1))
    if [ "$name" != PKG_CONFIG_PATH ] && [[ "$name" =~ $CHANGES_BUILD ]]; then
      echo "$name is set in the environment and would change what is built: unset it and build again" >&2
      exit 1
    fi
  done < <(env -0)
  [ "$names" -gt 0 ] || { echo "cannot read the environment (env -0)" >&2; exit 1; }
  # The flags .cargo/config.toml gives, which RUSTFLAGS would replace, and
  # room in the load commands for bundle-macos-libs.py to rewrite them.
  export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"
  export RUSTFLAGS="-L native=$FFMPEG_FULL/lib -L native=$BREW/lib -C link-arg=-Wl,-headerpad_max_install_names"
  export PKG_CONFIG_PATH="$FFMPEG_FULL/lib/pkgconfig"
  # Where cargo puts the program, asked of cargo; one left there by an
  # earlier build goes first: what is there afterwards, this build made.
  TARGET_DIR="$(cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  [ -n "$TARGET_DIR" ] || { echo "cargo does not say where it builds" >&2; exit 1; }
  BINARY="$TARGET_DIR/release/varda"
  rm -f "$BINARY"
  # shellcheck disable=SC2086
  cargo build --release --locked $FEATURES
  if [ ! -f "$BINARY" ] || [ ! -x "$BINARY" ]; then
    echo "cargo built, and no program is at $BINARY" >&2
    exit 1
  fi
fi
if [ "$BUILD_ONLY" = true ]; then
  BINARY_DIR="$(cd "$(dirname "$BINARY")" && pwd -P)"
  echo "==> Built, not packaged: $BINARY_DIR/$(basename "$BINARY")"
  exit 0
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/Nida.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/licenses"
cp "$BINARY" "$APP/Contents/MacOS/nida"
cp -L "$FFMPEG_FULL/bin/ffmpeg" "$APP/Contents/MacOS/ffmpeg"
chmod 755 "$APP/Contents/MacOS/ffmpeg"
mkdir -p "$APP/Contents/Resources/shaders"
rsync -a --exclude-from=<(grep -v -E '^(#|$)' scripts/ci/shaders-left-out.txt) \
  shaders/ "$APP/Contents/Resources/shaders/"
cp LICENSE "$APP/Contents/Resources/licenses/LICENSE-Nida-MIT"
cp "$FFMPEG_FULL/COPYING.GPLv3" "$APP/Contents/Resources/licenses/COPYING-GPLv3"
# WebRTC's echo canceller, linked into nida (vendor/webrtc-aec3): its
# licences, its patent grant, and which source it was built from.
mkdir -p "$APP/Contents/Resources/licenses/webrtc-aec3"
cp vendor/webrtc-aec3/licenses/*.txt vendor/webrtc-aec3/README.md \
  "$APP/Contents/Resources/licenses/webrtc-aec3/"

# The icon, at every size macOS asks for.
ICONSET="$STAGE/nida.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/nida.icns"

# As make-local-app.sh's, without this computer's settings: the shaders are
# found in Resources, the API port is the default, and no command-line tool
# is offered (it asks for an administrator's password).
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Nida</string>
  <key>CFBundleDisplayName</key><string>Nida</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key><string>${BUILD}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>nida</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>nida.icns</string>
  <key>LSMinimumSystemVersion</key><string>${MIN_MACOS}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSCameraUsageDescription</key><string>Nida shows cameras and capture cards as inputs.</string>
  <key>NSCameraReactionEffectGesturesEnabledDefault</key><false/>
  <key>NSMicrophoneUsageDescription</key><string>Nida mixes microphones and other sound inputs.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Nida takes an app's sound, such as a Zoom or Teams call's, as an input to mix.</string>
  <key>NSAppleEventsUsageDescription</key><string>Nida plays PowerPoint decks as inputs: it opens them in PowerPoint and moves the show on when you click.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Nida finds and sends NDI video on the local network.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_ndi._tcp</string>
  </array>
  <key>LSEnvironment</key>
  <dict>
    <key>VARDA_NO_CLI_INSTALL</key><string>1</string>
  </dict>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
SOURCE_TAR="Nida-$VERSION-third-party-source.tar.gz"
SOURCE_ARGS=()
[ "$SOURCES" = true ] && SOURCE_ARGS=(--sources "$OUT/$SOURCE_TAR")
python3 scripts/ci/bundle-macos-libs.py "$APP" --min-macos "$MIN_MACOS" \
  --identifier "$BUNDLE_ID" --origin "nida=$BINARY" --origin "ffmpeg=$FFMPEG_FULL/bin/ffmpeg" \
  --cargo-args "$FEATURES" --version "$VERSION" ${SOURCE_ARGS[@]+"${SOURCE_ARGS[@]}"}

# Both programs start, every library found inside the app.
VARDA_NO_CLI_INSTALL=1 "$APP/Contents/MacOS/nida" --version
"$APP/Contents/MacOS/ffmpeg" -hide_banner -version | head -1
for program in nida ffmpeg; do
  arg=--version
  [ "$program" = ffmpeg ] && arg=-version
  loaded="$(DYLD_PRINT_LIBRARIES=1 VARDA_NO_CLI_INSTALL=1 "$APP/Contents/MacOS/$program" "$arg" 2>&1 >/dev/null || true)"
  if grep -q -E '/opt/homebrew|/usr/local' <<<"$loaded"; then
    echo "$program loaded from outside the app:" >&2
    grep -E '/opt/homebrew|/usr/local' <<<"$loaded" >&2
    exit 1
  fi
done

DMG_DIR="$STAGE/dmg"
mkdir -p "$DMG_DIR"
ditto "$APP" "$DMG_DIR/Nida.app"
ln -s /Applications "$DMG_DIR/Applications"
cat > "$DMG_DIR/Read me first.txt" <<TXT
Nida $BUILD — for Macs with Apple silicon, macOS Sequoia $MIN_MACOS or later

1. Drag Nida to Applications.
2. Open it. The first time, macOS says it cannot check Nida for malicious
   software: Nida is not signed by Apple. Open System Settings → Privacy &
   Security, and next to "Nida was blocked" click Open Anyway. Only once.
3. For NDI, install the free NDI runtime from ndi.video: the NDI SDK for
   Apple, or with Homebrew: brew install --cask libndi

Manual: https://github.com/kristijonasatpro/Nida/blob/main/docs/README.md

Nida's own code is under the MIT licence. Built with FFmpeg under the GPL,
the app as a whole is given away under the GPL version 3: see
Nida.app/Contents/Resources/licenses. Its source: Nida's at
https://github.com/kristijonasatpro/Nida/tree/v$VERSION, and that of every
library inside in $SOURCE_TAR on the release page,
https://github.com/kristijonasatpro/Nida/releases/tag/v$VERSION.

vMix and Resolume are trademarks of their owners; Nida is not affiliated
with either.
TXT

DMG="$OUT/Nida-$BUILD-macOS-arm64.dmg"
rm -f "$DMG"
# hdiutil is known to fail now and then with "Resource busy" on CI Macs.
for attempt in 1 2 3; do
  if hdiutil create -volname "Nida $BUILD" -srcfolder "$DMG_DIR" -ov -format UDZO "$DMG" >/dev/null; then
    break
  fi
  [ "$attempt" = 3 ] && exit 1
  sleep 5
done
# Named as it is downloaded, so `shasum -c` works beside it.
(cd "$OUT" && shasum -a 256 "$(basename "$DMG")" | tee "$(basename "$DMG").sha256")
ls -lh "$OUT"/Nida-"$VERSION"-*
