#!/usr/bin/env bash
# Build clipd and produce Clipd.app — double-click to run (GUI + daemon). One step for users.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

APP_VERSION="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"

echo "==> cargo build --release"
cargo build --release

echo "==> clipd-hud (Swift overlay — required for HUD inside .app)"
if [[ ! -f target/release/clipd-hud ]] && command -v swiftc &>/dev/null; then
  (cd clipd-hud && swiftc -O -o clipd-hud clipd-hud.swift -framework Cocoa)
  cp -f clipd-hud/clipd-hud target/release/clipd-hud
  chmod +x target/release/clipd-hud
fi
if command -v swiftc &>/dev/null && [[ ! -f target/release/clipd-hud ]]; then
  echo ""
  echo "  ERROR: clipd-hud was not built but swiftc is available."
  echo "  Fix: cd clipd-hud && swiftc -O -o ../target/release/clipd-hud clipd-hud.swift -framework Cocoa"
  exit 1
fi
if [[ ! -f target/release/clipd-hud ]]; then
  echo "    (warning: no clipd-hud — install Xcode CLI tools; HUD will not work in this bundle)"
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

# Ad-hoc sign helpers so macOS allows the daemon to spawn clipd-hud (unsigned helper is often blocked).
echo "==> codesign (ad-hoc) — MacOS binaries + app bundle"
if command -v codesign &>/dev/null; then
  for bin in clipd clipd-gui clipd-ui clipd-hud; do
    [[ -f "$MACOS/$bin" ]] || continue
    codesign --force --sign - "$MACOS/$bin" 2>/dev/null || true
  done
  codesign --force --deep --sign - "$APP" 2>/dev/null || true
else
  echo "    (skip: codesign not found)"
fi

# Strip quarantine so Finder-launched copies do not block helper binaries (clipd-hud) as harshly.
if command -v xattr &>/dev/null; then
  xattr -cr "$APP" 2>/dev/null || true
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
  <string>${APP_VERSION}</string>
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
