#!/usr/bin/env bash
# Build clipd and package for the CURRENT platform (macOS or Linux).
#
# Usage:
#   ./dist.sh              # build + package (version from Cargo.toml)
#   ./dist.sh v0.2.0       # override version tag
#
# Produces:
#   macOS  → target/release/Clipd.app + Clipd-macos-<arch>-<ver>.zip
#   Linux  → Clipd-linux-<arch>-<ver>.tar.gz  (with .desktop file, launcher)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

VERSION="${1:-v$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
ARCH="$(uname -m)"
case "$ARCH" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64)        ARCH="x86_64" ;;
  *)             echo "Unsupported arch: $ARCH"; exit 1 ;;
esac

OS="$(uname -s)"
case "$OS" in
  Darwin) PLATFORM="macos" ;;
  Linux)  PLATFORM="linux" ;;
  *)      echo "Unsupported OS: $OS. Use dist.ps1 on Windows."; exit 1 ;;
esac

PKG_NAME="Clipd-${PLATFORM}-${ARCH}-${VERSION}"

echo "╔════════════════════════════════════════════╗"
echo "║  clipd dist — ${VERSION}  (${PLATFORM}/${ARCH})"
echo "╚════════════════════════════════════════════╝"
echo ""

# ── 1. Rust release build ──
echo "==> cargo build --release"
cargo build --release

# ── 2. Platform-specific packaging ──
if [ "$PLATFORM" = "macos" ]; then
  package_macos
else
  package_linux
fi

echo ""
echo "════════════════════════════════════════════"
echo "  Done!  Artifacts in target/release/:"
echo ""
if [ "$PLATFORM" = "macos" ]; then
  echo "  Clipd.app              — drag to /Applications"
fi
echo "  ${PKG_NAME}.zip*       — upload to GitHub Releases"
echo "════════════════════════════════════════════"

# ──────────────────────────────────────────────
# macOS packaging
# ──────────────────────────────────────────────
package_macos() {
  echo "==> clipd-hud (Swift overlay)"
  if command -v swiftc &>/dev/null; then
    (cd clipd-hud && swiftc -O -o clipd-hud clipd-hud.swift -framework Cocoa)
    cp -f clipd-hud/clipd-hud target/release/clipd-hud
    chmod +x target/release/clipd-hud
  else
    echo "    ⚠ swiftc not found — HUD overlay skipped."
    echo "      Install Xcode CLI tools: xcode-select --install"
  fi

  echo "==> Assembling Clipd.app"
  bash packaging/macos/create-app-bundle.sh

  APP="target/release/Clipd.app"
  if command -v codesign &>/dev/null; then
    echo "==> Code signing (ad-hoc)"
    codesign --force --deep --sign - "$APP" 2>/dev/null || true
  fi

  echo "==> Creating ${PKG_NAME}.zip"
  STAGE="target/release/${PKG_NAME}"
  rm -rf "$STAGE" "target/release/${PKG_NAME}.zip"
  mkdir -p "$STAGE"

  cp -R "$APP" "$STAGE/"
  cp -f target/release/clipd "$STAGE/"
  chmod +x "$STAGE/clipd"
  cp -f install.sh README.md "$STAGE/" 2>/dev/null || true

  (cd target/release && zip -qr "${PKG_NAME}.zip" "${PKG_NAME}/")
  rm -rf "$STAGE"
}

# ──────────────────────────────────────────────
# Linux packaging
# ──────────────────────────────────────────────
package_linux() {
  echo "==> Creating ${PKG_NAME}.zip"

  STAGE="target/release/${PKG_NAME}"
  rm -rf "$STAGE" "target/release/${PKG_NAME}.zip"
  mkdir -p "$STAGE"

  for bin in clipd clipd-ui clipd-gui clipd-mcp; do
    if [ -f "target/release/${bin}" ]; then
      cp -f "target/release/${bin}" "$STAGE/"
      chmod +x "$STAGE/${bin}"
    fi
  done

  cp -f install.sh README.md "$STAGE/" 2>/dev/null || true

  mkdir -p "$STAGE/packaging/linux"
  cp -f packaging/linux/clipd.desktop "$STAGE/"
  cp -f packaging/linux/install-desktop.sh "$STAGE/"
  chmod +x "$STAGE/install-desktop.sh"

  cat > "$STAGE/clipd-launch.sh" << 'LAUNCH'
#!/usr/bin/env bash
# Double-click launcher for clipd on Linux.
# Starts the tray UI which spawns the daemon + GUI.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
export PATH="$SCRIPT_DIR:$PATH"
exec "$SCRIPT_DIR/clipd-ui"
LAUNCH
  chmod +x "$STAGE/clipd-launch.sh"

  (cd target/release && zip -qr "${PKG_NAME}.zip" "${PKG_NAME}/")
  rm -rf "$STAGE"
}
