#!/bin/bash
# Install Clipd — downloads the latest release from GitHub.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
#
# Supports macOS (arm64/x86_64) and Linux (x86_64).
# For Windows, use install.ps1 instead.
#
# This script:
#   1. Detects your OS and architecture
#   2. Downloads the latest release
#   3. Installs Clipd (macOS: .app to Applications; Linux: binaries to ~/.local/bin)
#   4. Installs required system libraries (Linux only, with sudo if available)
#   5. Sets up desktop integration (.desktop file, icon)
#   6. Configures autostart so Clipd launches on login
#   7. Adds Clipd to your PATH

set -e

REPO="shwetarkadam/clipd"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "  ${CYAN}→${NC} $*"; }
ok()    { echo -e "  ${GREEN}✓${NC} $*"; }
warn()  { echo -e "  ${YELLOW}⚠${NC} $*"; }
err()   { echo -e "  ${RED}✗${NC} $*"; }

# ──────────────────────────────────────────────
# Functions (defined before use)
# ──────────────────────────────────────────────

add_to_path() {
  local dir="$1"
  local shell_rc=""

  if [ -n "${ZSH_VERSION:-}" ]; then
    shell_rc="${HOME}/.zshrc"
  elif [ -n "${BASH_VERSION:-}" ]; then
    if [ -f "${HOME}/.bashrc" ]; then
      shell_rc="${HOME}/.bashrc"
    elif [ -f "${HOME}/.bash_profile" ]; then
      shell_rc="${HOME}/.bash_profile"
    fi
  fi

  if [ -n "$shell_rc" ] && [ -f "$shell_rc" ] && ! grep -q "$dir" "$shell_rc" 2>/dev/null; then
    echo "" >> "$shell_rc"
    echo "export PATH=\"\$PATH:$dir\"  # clipd" >> "$shell_rc"
    ok "Added $dir to PATH in $(basename "$shell_rc")"
    export PATH="$PATH:$dir"
  elif [ -n "$shell_rc" ]; then
    ok "$dir already in PATH"
  fi
}

install_linux_deps() {
  info "Checking system libraries..."

  local needs_deps=false
  for lib in libgtk-3.so.0 libappindicator3.so.1 libayatana-appindicator3.so.1; do
    if ! ldconfig -p 2>/dev/null | grep -q "$lib" && ! find /usr/lib -name "$lib" 2>/dev/null | grep -q .; then
      needs_deps=true
      break
    fi
  done

  if [ "$needs_deps" = false ]; then
    ok "System libraries already installed"
    return
  fi

  info "Installing required system libraries..."

  if command -v apt-get &>/dev/null; then
    if [ -w /usr/bin ] || sudo -n true 2>/dev/null; then
      sudo apt-get update -qq
      sudo apt-get install -y -qq \
        libgtk-3-0 libayatana-appindicator3-1 libdbus-1-3 libxdo3 2>/dev/null || \
      warn "Could not install libraries. Run manually:"
      echo "    sudo apt-get install -y libgtk-3-0 libayatana-appindicator3-1 libdbus-1-3 libxdo3"
    else
      warn "Need sudo to install libraries. Run manually:"
      echo "    sudo apt-get install -y libgtk-3-0 libayatana-appindicator3-1 libdbus-1-3 libxdo3"
    fi
  elif command -v dnf &>/dev/null; then
    sudo dnf install -y gtk3 libappindicator-gtk3 libdbus libxdo 2>/dev/null || \
    warn "Could not install libraries. Run manually:"
    echo "    sudo dnf install -y gtk3 libappindicator-gtk3 libdbus libxdo"
  elif command -v pacman &>/dev/null; then
    sudo pacman -S --noconfirm gtk3 libappindicator libdbus libxdo 2>/dev/null || \
    warn "Could not install libraries. Run manually:"
    echo "    sudo pacman -S gtk3 libappindicator libdbus libxdo"
  elif command -v zypper &>/dev/null; then
    sudo zypper install -y gtk3 libappindicator3-1 libdbus-1-3 libxdo3 2>/dev/null || \
    warn "Could not install libraries. Run manually:"
    echo "    sudo zypper install -y gtk3 libappindicator3-1 libdbus-1-3 libxdo3"
  else
    warn "Unknown package manager. Install these libraries manually:"
    echo "    GTK3, appindicator/Ayatana, D-Bus, libxdo"
  fi
}

setup_linux_desktop() {
  local INSTALL_DIR="${HOME}/.local/bin"
  local APPS_DIR="${HOME}/.local/share/applications"
  local ICONS_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

  mkdir -p "$APPS_DIR" "$ICONS_DIR"

  if [ -f "$SRC/clipd.svg" ]; then
    cp -f "$SRC/clipd.svg" "${ICONS_DIR}/clipd.svg"
  elif [ -f "packaging/icons/clipd.svg" ]; then
    cp -f packaging/icons/clipd.svg "${ICONS_DIR}/clipd.svg"
  fi

  cat > "${APPS_DIR}/clipd.desktop" << DESKEOF
[Desktop Entry]
Type=Application
Name=Clipd
GenericName=Clipboard Manager
Comment=AI-powered multi-slot clipboard manager
Exec=${INSTALL_DIR}/clipd-ui
Icon=clipd
Terminal=false
Categories=Utility;Office;System;
Keywords=clipboard;manager;copy;paste;multi-slot;
StartupNotify=true
DESKEOF

  update-desktop-database "$APPS_DIR" 2>/dev/null || true
  ok "Desktop integration configured (.desktop file + icon)"
}

setup_linux_autostart() {
  local AUTOSTART_DIR="${HOME}/.config/autostart"
  local INSTALL_DIR="${HOME}/.local/bin"

  mkdir -p "$AUTOSTART_DIR"

  cat > "${AUTOSTART_DIR}/clipd.desktop" << AUTOSTARTEOF
[Desktop Entry]
Type=Application
Name=Clipd
Exec=${INSTALL_DIR}/clipd-ui
Icon=clipd
Terminal=false
Hidden=false
X-GNOME-Autostart-enabled=true
Comment=Clipboard manager — starts tray icon and daemon
AUTOSTARTEOF

  ok "Auto-start configured (XDG autostart)"
}

setup_macos_autostart() {
  local app_path="$1"
  if [ -z "$app_path" ]; then
    return
  fi

  local PLIST_DIR="${HOME}/Library/LaunchAgents"
  local PLIST="${PLIST_DIR}/dev.clipd.autostart.plist"
  local EXEC="${app_path}/Contents/MacOS/clipd-ui"

  mkdir -p "$PLIST_DIR"

  cat > "$PLIST" << PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.clipd.autostart</string>
  <key>ProgramArguments</key>
  <array>
    <string>${EXEC}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>StandardOutPath</key>
  <string>${HOME}/Library/Logs/clipd-autostart.log</string>
  <key>StandardErrorPath</key>
  <string>${HOME}/Library/Logs/clipd-autostart.log</string>
</dict>
</plist>
PLISTEOF

  launchctl unload "$PLIST" 2>/dev/null || true
  launchctl load "$PLIST" 2>/dev/null || true
  ok "Auto-start configured (LaunchAgent)"
}

install_macos() {
  if [ -d "$SRC/Clipd.app" ]; then
    if sudo rm -rf "/Applications/Clipd.app" 2>/dev/null && sudo cp -R "$SRC/Clipd.app" "/Applications/" 2>/dev/null; then
      ok "Copied Clipd.app → /Applications/"
      APP_PATH="/Applications/Clipd.app"
    else
      mkdir -p "${HOME}/Applications"
      rm -rf "${HOME}/Applications/Clipd.app"
      cp -R "$SRC/Clipd.app" "${HOME}/Applications/"
      ok "Copied Clipd.app → ~/Applications/"
      APP_PATH="${HOME}/Applications/Clipd.app"
    fi

    if command -v xattr &>/dev/null; then
      xattr -cr "$APP_PATH" 2>/dev/null || true
    fi
  else
    warn "Clipd.app not found in release zip"
    APP_PATH=""
  fi

  INSTALL_DIR="/usr/local/bin"
  if [ -f "$SRC/clipd" ]; then
    if sudo mkdir -p "$INSTALL_DIR" 2>/dev/null && sudo cp -f "$SRC/clipd" "${INSTALL_DIR}/clipd" 2>/dev/null && sudo chmod +x "${INSTALL_DIR}/clipd" 2>/dev/null; then
      ok "Copied clipd CLI → ${INSTALL_DIR}/clipd"
    else
      INSTALL_DIR="${HOME}/.local/bin"
      mkdir -p "$INSTALL_DIR"
      cp -f "$SRC/clipd" "${INSTALL_DIR}/clipd"
      chmod +x "${INSTALL_DIR}/clipd"
      ok "Copied clipd CLI → ${INSTALL_DIR}/clipd"
      add_to_path "$INSTALL_DIR"
    fi
  fi

  setup_macos_autostart "$APP_PATH"

  echo ""
  ok "Done! Clipd ${VERSION} installed."
  echo ""
  echo "  Next steps:"
  if [ -n "$APP_PATH" ]; then
    echo "    1. Open Clipd from Applications or Spotlight"
  fi
  echo "    2. macOS will ask for permissions — grant them:"
  echo "       - Accessibility (System Settings → Privacy & Security → Accessibility)"
  echo "       - Input Monitoring (System Settings → Privacy & Security → Input Monitoring)"
  echo "    3. Restart Clipd after granting permissions"
  echo "    4. Clipd will auto-start on login (LaunchAgent)"
  echo ""
  echo "  CLI: clipd list | clipd search | clipd slots"
  echo ""
}

install_linux() {
  local INSTALL_DIR="${HOME}/.local/bin"
  mkdir -p "$INSTALL_DIR"

  for bin in clipd clipd-ui clipd-gui clipd-mcp; do
    if [ -f "$SRC/${bin}" ]; then
      cp -f "$SRC/${bin}" "${INSTALL_DIR}/${bin}"
      chmod +x "${INSTALL_DIR}/${bin}"
      ok "Installed ${bin} → ${INSTALL_DIR}/"
    fi
  done

  add_to_path "$INSTALL_DIR"
  install_linux_deps
  setup_linux_desktop
  setup_linux_autostart
}

# ──────────────────────────────────────────────
# Main
# ──────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)             err "Unsupported architecture: $ARCH"; exit 1 ;;
esac

case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux)  PLATFORM="linux" ;;
  *)      err "Unsupported OS: $OS (use install.ps1 on Windows)"; exit 1 ;;
esac

if [ -n "${CLIPD_VERSION:-}" ]; then
  VERSION="$CLIPD_VERSION"
else
  info "Fetching latest release..."
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  if [ -z "$VERSION" ]; then
    err "Could not determine latest version. Set CLIPD_VERSION and retry."
    exit 1
  fi
fi

echo ""
echo -e "  ${BOLD}Clipd ${VERSION}${NC} (${PLATFORM}/${ARCH})"
echo ""

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

ZIP_NAME="Clipd-${PLATFORM}-${ARCH}-${VERSION}"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ZIP_NAME}.zip"

info "Downloading ${URL}..."
curl -fSL "$URL" -o "$TMP/clipd.zip"
unzip -qo "$TMP/clipd.zip" -d "$TMP"

SRC="$TMP/${ZIP_NAME}"
if [ ! -d "$SRC" ]; then
  err "Expected folder ${ZIP_NAME} inside zip"
  exit 1
fi

if [ "$PLATFORM" = "macos" ]; then
  install_macos
  exit 0
fi

install_linux

echo ""
ok "Done! Clipd ${VERSION} installed."
echo ""
echo "  Next steps:"
echo "    1. Open a new terminal (or run: source ~/.profile)"
echo "    2. Clipd will auto-start on login"
echo "    3. Or run now: ${HOME}/.local/bin/clipd-ui &"
echo ""
echo "  CLI: clipd list | clipd search | clipd slots"
echo ""
