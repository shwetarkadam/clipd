#!/bin/bash
# Install Clipd — downloads the latest release from GitHub.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
#
# Supports macOS (arm64/x86_64) and Linux (x86_64).
# For Windows, use install.ps1 instead.

set -e

REPO="shwetarkadam/clipd"

# ── Detect OS & architecture ──
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)             echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux)  PLATFORM="linux" ;;
  *)      echo "Unsupported OS: $OS (use install.ps1 on Windows)"; exit 1 ;;
esac

# ── Determine latest version (or use $CLIPD_VERSION) ──
if [ -n "${CLIPD_VERSION:-}" ]; then
  VERSION="$CLIPD_VERSION"
else
  echo "Fetching latest release..."
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  if [ -z "$VERSION" ]; then
    echo "Could not determine latest version. Set CLIPD_VERSION and retry."
    exit 1
  fi
fi

ZIP_NAME="Clipd-${PLATFORM}-${ARCH}-${VERSION}"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ZIP_NAME}.zip"

echo ""
echo "  Installing Clipd ${VERSION} (${PLATFORM}/${ARCH})..."
echo "  ${URL}"
echo ""

# ── Download ──
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fSL "$URL" -o "$TMP/clipd.zip"
unzip -qo "$TMP/clipd.zip" -d "$TMP"

SRC="$TMP/${ZIP_NAME}"
if [ ! -d "$SRC" ]; then
  echo "Error: expected folder ${ZIP_NAME} inside zip"
  exit 1
fi

# ── macOS-specific: install Clipd.app ──
if [ "$PLATFORM" = "macos" ]; then
  APP_DIR="/Applications"
  if [ -d "$SRC/Clipd.app" ]; then
    echo "  Copying Clipd.app → ${APP_DIR}/"
    rm -rf "${APP_DIR}/Clipd.app"
    cp -R "$SRC/Clipd.app" "${APP_DIR}/"
  else
    echo "  Warning: Clipd.app not found in release zip"
  fi
fi

# ── Install CLI binary ──
if [ "$PLATFORM" = "macos" ]; then
  INSTALL_DIR="/usr/local/bin"
else
  INSTALL_DIR="$HOME/.local/bin"
fi

if [ -f "$SRC/clipd" ]; then
  echo "  Copying clipd CLI → ${INSTALL_DIR}/"
  if [ "$PLATFORM" = "macos" ]; then
    sudo mkdir -p "$INSTALL_DIR"
    sudo cp -f "$SRC/clipd" "${INSTALL_DIR}/clipd"
    sudo chmod +x "${INSTALL_DIR}/clipd"
  else
    mkdir -p "$INSTALL_DIR"
    cp -f "$SRC/clipd" "${INSTALL_DIR}/clipd"
    chmod +x "${INSTALL_DIR}/clipd"
  fi
fi

# ── Linux: also install tray binary ──
if [ "$PLATFORM" = "linux" ] && [ -f "$SRC/clipd-ui" ]; then
  echo "  Copying clipd-ui → ${INSTALL_DIR}/"
  cp -f "$SRC/clipd-ui" "${INSTALL_DIR}/clipd-ui"
  chmod +x "${INSTALL_DIR}/clipd-ui"
fi

echo ""
echo "  Done! Clipd installed."
echo ""

if [ "$PLATFORM" = "macos" ]; then
  echo "  Next steps:"
  echo "    1. Open Clipd from Applications (or Spotlight)"
  echo "    2. macOS will ask for permissions — grant them:"
  echo "       - Accessibility (for paste simulation)"
  echo "       - Input Monitoring (for hotkey detection)"
  echo "    3. The menu bar icon appears — you're ready!"
elif [ "$PLATFORM" = "linux" ]; then
  echo "  Next steps:"
  echo "    1. Make sure ~/.local/bin is in your PATH"
  echo "       export PATH=\"\$HOME/.local/bin:\$PATH\""
  echo "    2. Start the daemon:  clipd daemon &"
  echo "    3. Start the tray:    clipd-ui &"
fi

echo ""
echo "  CLI: clipd list | clipd search | clipd slots"
echo ""
