#!/bin/bash
# Install Clipd on macOS — downloads the latest release from GitHub.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/shwetarkadam/clipd/main/install.sh | bash
#
# What it does:
#   1. Downloads Clipd-macos-<arch>-<version>.zip from GitHub Releases
#   2. Copies Clipd.app into /Applications
#   3. Copies the `clipd` CLI into /usr/local/bin
#   4. Grants execute permissions
#
# After install, open Clipd from Applications (or Spotlight).
# First launch: macOS will ask for Accessibility + Input Monitoring permissions.

set -e

REPO="shwetarkadam/clipd"
INSTALL_DIR="/usr/local/bin"
APP_DIR="/Applications"

# ── Detect architecture ──
ARCH="$(uname -m)"
case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)             echo "Unsupported architecture: $ARCH"; exit 1 ;;
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

ZIP_NAME="Clipd-macos-${ARCH}-${VERSION}"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ZIP_NAME}.zip"

echo ""
echo "  Installing Clipd ${VERSION} (${ARCH})..."
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

# ── Install Clipd.app ──
if [ -d "$SRC/Clipd.app" ]; then
  echo "  Copying Clipd.app → ${APP_DIR}/"
  # Remove old version first
  rm -rf "${APP_DIR}/Clipd.app"
  cp -R "$SRC/Clipd.app" "${APP_DIR}/"
else
  echo "  Warning: Clipd.app not found in release zip"
fi

# ── Install CLI ──
if [ -f "$SRC/clipd" ]; then
  echo "  Copying clipd CLI → ${INSTALL_DIR}/"
  sudo mkdir -p "$INSTALL_DIR"
  sudo cp -f "$SRC/clipd" "${INSTALL_DIR}/clipd"
  sudo chmod +x "${INSTALL_DIR}/clipd"
fi

echo ""
echo "  ✅ Clipd installed!"
echo ""
echo "  Next steps:"
echo "    1. Open Clipd from Applications (or Spotlight)"
echo "    2. macOS will ask for permissions — grant them:"
echo "       • Accessibility (for paste simulation)"
echo "       • Input Monitoring (for hotkey detection)"
echo "    3. The menu bar icon (📋) appears — you're ready!"
echo ""
echo "  Quick test: copy something, then Ctrl+C to save to slot 1"
echo "  CLI: clipd list | clipd search | clipd slots"
echo ""
