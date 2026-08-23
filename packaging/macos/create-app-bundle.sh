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

# Info.plist MUST be written before codesign — writing it after invalidates
# the sealed signature and launchd fails with POSIX 163.
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
  <!-- Local Network — required on macOS 15+ to find your other machines over
       Bonjour and send clips straight to them. Without NSBonjourServices the
       browse returns nothing at all, silently: no error, no peers, ever. Every
       service type clipd browses for MUST be listed below or it is invisible. -->
  <key>NSLocalNetworkUsageDescription</key>
  <string>Clipd finds your other computers on this network so you can send clips, links and files straight to them. Nothing leaves your local network.</string>
  <key>NSBonjourServices</key>
  <array>
    <!-- Everyday discovery: which of your machines are on this network. -->
    <string>_clipd._tcp</string>
    <!-- Only advertised while `clipd pair` is running. -->
    <string>_clipd-pair._tcp</string>
  </array>
  <!-- Menu-bar agent: no Dock icon. Status item stays owned by clipd-ui when
       other apps (e.g. Sublime) become frontmost. TCC prompts still work. -->
  <key>LSUIElement</key>
  <true/>
  <key>LSBackgroundOnly</key>
  <false/>
</dict>
</plist>
PLIST


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
#
# Since that failure is silent and costs an afternoon to diagnose, an identity in
# the login keychain is used automatically. Set CLIPD_SIGN_ID="-" to force ad-hoc.
pick_signing_identity() {
  local list
  list="$(security find-identity -v -p codesigning 2>/dev/null)" || return 0
  local preference
  # Developer ID first (also valid for distribution), then a dev cert, then any.
  for preference in 'Developer ID Application' 'Apple Development' ''; do
    local found
    found="$(printf '%s\n' "$list" \
      | grep -F "\"${preference}" \
      | head -1 \
      | sed -E 's/^[^"]*"(.*)".*$/\1/')"
    if [[ -n "$found" ]]; then
      printf '%s' "$found"
      return 0
    fi
  done
}

if [[ -n "${CLIPD_SIGN_ID+isset}" ]]; then
  SIGN_ID="$CLIPD_SIGN_ID"          # explicit wins, including "-" for ad-hoc
else
  SIGN_ID="$(pick_signing_identity)"
  SIGN_ID="${SIGN_ID:--}"
fi

if [[ "$SIGN_ID" == "-" ]]; then
  echo "==> codesign (ad-hoc — TCC grants will NOT persist across rebuilds:"
  echo "    macOS will silently drop Input Monitoring on the next build.)"
  if [[ -z "${CLIPD_SIGN_ID+isset}" ]]; then
    echo "    No signing identity found. Create a self-signed cert in Keychain"
    echo "    Access named clipd-codesign, or set CLIPD_SIGN_ID."
  fi
else
  echo "==> codesign (identity: ${SIGN_ID} — TCC grants persist across rebuilds)"
fi
if command -v codesign &>/dev/null; then
  for bin in clipd clipd-gui clipd-ui clipd-hud clipd-ocr; do
    [[ -f "$MACOS/$bin" ]] || continue
    if ! codesign --force --sign "$SIGN_ID" "$MACOS/$bin"; then
      echo "ERROR: codesign failed for $MACOS/$bin using '$SIGN_ID'" >&2
      echo "       The app was not packaged because a changing ad-hoc signature" >&2
      echo "       would silently break Input Monitoring and slot HUD feedback." >&2
      exit 1
    fi
  done
  if ! codesign --force --deep --sign "$SIGN_ID" "$APP"; then
    echo "ERROR: codesign failed for $APP using '$SIGN_ID'" >&2
    exit 1
  fi
else
  echo "    (skip: codesign not found)"
fi

# Strip quarantine so Finder-launched copies do not block helper binaries (clipd-hud) as harshly.
if command -v xattr &>/dev/null; then
  xattr -cr "$APP" 2>/dev/null || true
fi

echo ""
echo "Built: $APP"
echo "Users: drag Clipd.app to Applications, double-click once."
echo "        Menu bar icon (clipd-ui) + main window + daemon."
echo "CLI:   $MACOS/clipd list"
