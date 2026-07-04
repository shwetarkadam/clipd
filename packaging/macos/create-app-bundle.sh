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

echo "==> clipd-ocr (Swift — Apple Vision OCR for image clips)"
if [[ ! -f target/release/clipd-ocr ]] && command -v swiftc &>/dev/null; then
  (cd clipd-ocr && swiftc -O -o clipd-ocr clipd-ocr.swift -framework Vision -framework AppKit)
  cp -f clipd-ocr/clipd-ocr target/release/clipd-ocr
  chmod +x target/release/clipd-ocr
fi
if [[ ! -f target/release/clipd-ocr ]]; then
  echo "    (warning: no clipd-ocr — image clips will be stored without searchable OCR text)"
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
if [[ -f target/release/clipd-ocr ]]; then
  cp -f target/release/clipd-ocr "$MACOS/"
  chmod +x "$MACOS/clipd-ocr"
fi

# Sign helpers so macOS allows the daemon to spawn clipd-hud, and — critically —
# so the Input Monitoring / Accessibility grants persist across updates.
#
# Set CLIPD_SIGN_ID to a stable code-signing identity (e.g. a self-signed
# "clipd-codesign" cert created in Keychain Access, or a Developer ID) so the
# app keeps the SAME code signature across rebuilds. macOS keys TCC grants to
# that identity, so users grant Input Monitoring once and it sticks.
#
# Without it we fall back to ad-hoc ("-"), whose signature hash changes every
# build — that makes macOS treat each build as a new app and silently drops the
# previously-granted Input Monitoring permission (multi-slot copy / HUD break).
SIGN_ID="${CLIPD_SIGN_ID:--}"
if [[ "$SIGN_ID" == "-" ]]; then
  echo "==> codesign (ad-hoc — grants will NOT persist across updates; set CLIPD_SIGN_ID to fix)"
else
  echo "==> codesign (identity: ${SIGN_ID} — TCC grants persist across updates)"
fi
if command -v codesign &>/dev/null; then
  for bin in clipd clipd-gui clipd-ui clipd-hud clipd-ocr; do
    [[ -f "$MACOS/$bin" ]] || continue
    codesign --force --sign "$SIGN_ID" "$MACOS/$bin" 2>/dev/null || true
  done
  codesign --force --deep --sign "$SIGN_ID" "$APP" 2>/dev/null || true
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
  <!-- Accessibility — required for global keyboard hook (multi-tap copy/paste slots) -->
  <key>NSAccessibilityUsageDescription</key>
  <string>Clipd needs Accessibility access to detect multi-tap ⌘C / ⌘V for clipboard slots. Without it, only single copy/paste works.</string>
  <!-- Input Monitoring — required on macOS 10.15+ for rdev keyboard events -->
  <key>NSInputMonitoringUsageDescription</key>
  <string>Clipd monitors keyboard shortcuts (⌘C, ⌘V) to save clipboard slots. No keystrokes are logged or sent anywhere.</string>
  <!-- AppleScript — used to open TUI in Terminal / Warp when Developer mode is on -->
  <key>NSAppleEventsUsageDescription</key>
  <string>Clipd uses AppleScript to open a terminal window for the developer TUI mode.</string>
  <!-- Run as a regular app (not just menu bar agent) so macOS prompts for permissions -->
  <key>LSUIElement</key>
  <false/>
  <key>LSBackgroundOnly</key>
  <false/>
</dict>
</plist>
PLIST

echo ""
echo "Built: $APP"
echo "Users: drag Clipd.app to Applications, double-click once."
echo "        Menu bar icon (clipd-ui) + main window + daemon."
echo "CLI:   $MACOS/clipd list"
