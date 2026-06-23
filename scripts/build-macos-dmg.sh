#!/usr/bin/env bash
#
# Assemble a universal Kuvatin.app and a .dmg from the two per-arch release
# binaries. Run on macOS (CI) after:
#   cargo build --release -p kuvatin --target aarch64-apple-darwin
#   cargo build --release -p kuvatin --target x86_64-apple-darwin
#
# Usage: scripts/build-macos-dmg.sh <version>
set -euo pipefail

VERSION="${1:?usage: build-macos-dmg.sh <version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ARM="target/aarch64-apple-darwin/release/kuvatin"
X64="target/x86_64-apple-darwin/release/kuvatin"
SVG="crates/kuvatin/assets/kuvatin-icon.svg"
PLIST="crates/kuvatin/macos/Info.plist"

DIST="dist"
APP="$DIST/Kuvatin.app"
DMG="$DIST/Kuvatin-${VERSION}-universal.dmg"

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# 1. Universal binary.
lipo -create -output "$APP/Contents/MacOS/kuvatin" "$ARM" "$X64"
chmod +x "$APP/Contents/MacOS/kuvatin"

# 2. Icon: render the SVG master at 1024, downscale into an .iconset, pack .icns.
ICONSET="$DIST/Kuvatin.iconset"
mkdir -p "$ICONSET"
rsvg-convert -w 1024 -h 1024 "$SVG" -o "$DIST/icon-1024.png"
for size in 16 32 64 128 256 512; do
  dbl=$((size * 2))
  sips -z "$size" "$size" "$DIST/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  sips -z "$dbl" "$dbl" "$DIST/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Kuvatin.icns"

# 3. Info.plist with the version substituted in.
sed "s/__VERSION__/${VERSION}/g" "$PLIST" > "$APP/Contents/Info.plist"

# 4. .dmg with a drag-to-Applications target.
STAGE="$DIST/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Kuvatin" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

echo "Built $DMG"
