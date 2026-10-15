#!/usr/bin/env bash
# Wrap this machine's release build in an app bundle, so it opens from the
# Finder or the Dock like any Mac app, and macOS asks *Nida* for the camera,
# the microphone, screen recording and the local network (NDI) — not the
# Terminal it would otherwise be started from.
#
# The bundle carries the settings a terminal would have passed: the API
# port, the workspace and extra shader folders, as environment variables the
# app reads (VARDA_PORT, VARDA_WORKSPACE, VARDA_SHADER_DIRS). It prints its
# log to ~/Library/Logs/Nida/nida.log. (Inside, the code and its settings
# still go by Varda, the project Nida is built on.)
#
# For this computer only: FFmpeg and the NDI runtime are used from where
# Homebrew put them, not bundled. For a DMG to give away, see build-macos.sh.
#
# macOS remembers what an app may use (camera, microphone, screen, folders)
# by its signature. Signed ad hoc, every build is a new app to it and asks
# again; signed with the same certificate each time, it asks once. Make one
# once in Keychain Access (Certificate Assistant → Create a Certificate…,
# name "Nida Local Signing", Self Signed Root, Code Signing) and this script
# signs with it; without it, it signs ad hoc.
#
# Usage: scripts/make-local-app.sh [--workspace DIR] [--port N]
#          [--shader-dir DIR] [--out DIR] [--id BUNDLE_ID] [--sign IDENTITY]
# Build first: cargo build --release --no-default-features --features screen-capture
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$HOME/Applications"
PORT=8080
WORKSPACE=""
SHADER_DIR="$ROOT/shaders"
BUNDLE_ID="lt.proconf.nida"
SIGN_IDENTITY="Nida Local Signing"
LOG_DIR="$HOME/Library/Logs/Nida"

while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) shift; WORKSPACE="$1" ;;
    --port) shift; PORT="$1" ;;
    --shader-dir) shift; SHADER_DIR="$1" ;;
    --out) shift; OUT="$1" ;;
    --id) shift; BUNDLE_ID="$1" ;;
    --sign) shift; SIGN_IDENTITY="$1" ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
  shift
done

BINARY="$ROOT/target/release/varda"
if [ ! -x "$BINARY" ]; then
  echo "No release build at $BINARY — build it first:" >&2
  echo "  cargo build --release --no-default-features --features screen-capture" >&2
  exit 1
fi

APP="$OUT/Nida.app"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/Nida.app/Contents/MacOS" "$STAGE/Nida.app/Contents/Resources" "$LOG_DIR"

cp "$BINARY" "$STAGE/Nida.app/Contents/MacOS/nida"

# The icon, at every size macOS asks for.
ICONSET="$STAGE/nida.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$ROOT/assets/icon.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$STAGE/Nida.app/Contents/Resources/nida.icns"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
# Built from changes not yet committed: say so, or two different builds share
# one label.
if ! git -C "$ROOT" diff --quiet HEAD -- 2>/dev/null; then
  COMMIT="${COMMIT}-dirty"
fi

# Environment the app starts with; a setting left empty is left out.
env_entry() {
  [ -n "$2" ] && printf '    <key>%s</key><string>%s</string>\n' "$1" "$2"
  return 0
}

# NSCameraReactionEffectGesturesEnabledDefault: macOS turns hand gestures in
# front of any camera into balloons, hearts or fireworks drawn into the
# picture — on air, for a live show. Off unless the user switches Gestures on
# in Control Center > Video Effects (macOS 14.4+; see AVCaptureDevice.h).
cat > "$STAGE/Nida.app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Nida</string>
  <key>CFBundleDisplayName</key><string>Nida</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key><string>${VERSION}-${COMMIT}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>nida</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>nida.icns</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSCameraUsageDescription</key><string>Nida shows cameras and capture cards as inputs.</string>
  <key>NSCameraReactionEffectGesturesEnabledDefault</key><false/>
  <key>NSMicrophoneUsageDescription</key><string>Nida mixes microphones and other sound inputs.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Nida takes an app's sound, such as a Zoom or Teams call's, as an input to mix.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Nida finds and sends NDI video on the local network.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_ndi._tcp</string>
  </array>
  <key>LSEnvironment</key>
  <dict>
$(env_entry VARDA_PORT "$PORT")
$(env_entry VARDA_WORKSPACE "$WORKSPACE")
$(env_entry VARDA_SHADER_DIRS "$SHADER_DIR")
$(env_entry VARDA_LOG_FILE "$LOG_DIR/nida.log")
$(env_entry VARDA_NO_CLI_INSTALL 1)
  </dict>
</dict>
</plist>
PLIST
plutil -lint "$STAGE/Nida.app/Contents/Info.plist" >/dev/null

# Signed with the certificate when there is one, so macOS keeps what it
# was allowed across builds; else ad hoc, and macOS asks again.
if security find-identity -p codesigning 2>/dev/null | grep -qF "\"$SIGN_IDENTITY\""; then
  codesign --force --sign "$SIGN_IDENTITY" --identifier "$BUNDLE_ID" "$STAGE/Nida.app"
  SIGNED="with \"$SIGN_IDENTITY\""
else
  codesign --force --sign - --identifier "$BUNDLE_ID" "$STAGE/Nida.app"
  SIGNED="ad hoc (no \"$SIGN_IDENTITY\" certificate: macOS will ask for permissions again)"
fi

mkdir -p "$OUT"
rm -rf "$APP"
mv "$STAGE/Nida.app" "$APP"
# Tell Launch Services about it, so the Dock and Spotlight find it at once.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP" 2>/dev/null || true

echo "Made $APP ($VERSION-$COMMIT)"
echo "  port $PORT, workspace ${WORKSPACE:-(default)}, log $LOG_DIR/nida.log"
echo "  signed $SIGNED"
