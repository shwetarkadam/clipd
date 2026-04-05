#!/usr/bin/env bash
# Build clipd and produce Clipd.app — double-click to run (GUI + daemon). One step for users.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

echo "==> cargo build --release"
cargo build --release

echo "==> clipd-hud (optional, for on-screen slot hints)"
if command -v swiftc &>/dev/null; then
  (cd clipd-hud && swiftc -O -o clipd-hud clipd-hud.swift -framework Cocoa)
  cp -f clipd-hud/clipd-hud target/release/clipd-hud
  chmod +x target/release/clipd-hud
else
  echo "    (skip: no swiftc — install Xcode CLI tools for HUD)"
fi

APP="target/release/Clipd.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"
mkdir -p "$MACOS" "$RES"

cp -f target/release/clipd target/release/clipd-gui target/release/clipd-ui "$MACOS/"
chmod +x "$MACOS/clipd" "$MACOS/clipd-gui" "$MACOS/clipd-ui"
if [[ -f target/release/clipd-hud ]]; then
  cp -f target/release/clipd-hud "$MACOS/"
  chmod +x "$MACOS/clipd-hud"
fi

# Menu bar + daemon + main window: clipd-ui is the entry (spawns daemon, opens clipd-gui).
# Dock / Finder use CFBundleExecutable; must match filename in MacOS/
EXEC_NAME="clipd-ui"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>${EXEC_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>dev.clipd.app</string>
  <key>CFBundleName</key>
  <string>Clipd</string>
  <key>CFBundleDisplayName</key>
  <string>Clipd</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo ""
echo "Built: $APP"
echo "Users: drag Clipd.app to Applications, double-click once."
echo "        Menu bar icon (clipd-ui) + main window + daemon."
echo "CLI:   $MACOS/clipd list"
