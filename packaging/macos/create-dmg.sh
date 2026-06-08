#!/usr/bin/env bash
# Create a macOS DMG for Clipd.
# Produces a drag-to-Applications DMG — the standard macOS install experience.
#
# Usage: ./create-dmg.sh [version]
# Produces: target/release/Clipd-<version>-<arch>.dmg
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
ARCH="$(uname -m)"
case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
esac

APP="target/release/Clipd.app"
if [ ! -d "$APP" ]; then
  echo "Error: $APP not found. Run create-app-bundle.sh first."
  exit 1
fi

DMG_NAME="Clipd-${VERSION}-${ARCH}"
DMG="target/release/${DMG_NAME}.dmg"
STAGING="target/release/${DMG_NAME}_dmg"

rm -rf "$STAGING" "$DMG"
mkdir -p "$STAGING"

cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"

if [ -f "target/release/clipd" ]; then
  cp -f target/release/clipd "$STAGING/"
  chmod +x "$STAGING/clipd"
fi

# Strip quarantine so Gatekeeper doesn't block the app after mount.
if command -v xattr &>/dev/null; then
  xattr -rc "$STAGING/Clipd.app" 2>/dev/null || true
fi

SIZE=$(du -sm "$STAGING" | cut -f1)
SIZE=$((SIZE + 20))

hdiutil create -volname "Clipd" \
  -srcfolder "$STAGING" \
  -ov -format UDZO \
  -imagekey zlib-level=9 \
  "$DMG"

rm -rf "$STAGING"
echo "Done: $DMG"
