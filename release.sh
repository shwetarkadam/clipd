#!/usr/bin/env bash
# Build clipd for release and package Clipd.app + CLI zip for GitHub Releases.
#
# Usage:
#   ./release.sh              # builds v0.1.0 (from Cargo.toml)
#   ./release.sh v0.2.0       # override version tag
#
# Produces:
#   target/release/Clipd.app              — drag-to-Applications bundle
#   target/release/Clipd-macos-<ver>.zip  — upload to GitHub Releases
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

VERSION="${1:-v$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
ARCH="$(uname -m)"   # arm64 or x86_64
ZIP_NAME="Clipd-macos-${ARCH}-${VERSION}"

echo "╔══════════════════════════════════════╗"
echo "║  clipd release build — ${VERSION}    "
echo "╚══════════════════════════════════════╝"
echo ""

# ── 1. Rust release build ──
echo "==> cargo build --release"
cargo build --release

# ── 2. Swift HUD overlay (optional) ──
echo "==> clipd-hud (Swift overlay)"
if command -v swiftc &>/dev/null; then
  (cd clipd-hud && swiftc -O -o clipd-hud clipd-hud.swift -framework Cocoa)
  cp -f clipd-hud/clipd-hud target/release/clipd-hud
  chmod +x target/release/clipd-hud
else
  echo "    ⚠ swiftc not found — HUD overlay will be skipped."
  echo "      Install Xcode Command Line Tools: xcode-select --install"
fi

# ── 3. Assemble Clipd.app bundle ──
echo "==> Assembling Clipd.app"
bash packaging/macos/create-app-bundle.sh

APP="target/release/Clipd.app"

# ── 4. Ad-hoc code sign (so Gatekeeper doesn't immediately block) ──
echo "==> Code signing (ad-hoc)"
codesign --force --deep --sign - "$APP" 2>/dev/null || echo "    (codesign skipped — install Xcode CLI tools)"

# ── 5. Create distributable zip ──
echo "==> Creating ${ZIP_NAME}.zip"
STAGE="target/release/${ZIP_NAME}"
rm -rf "$STAGE" "target/release/${ZIP_NAME}.zip"
mkdir -p "$STAGE"

# App bundle for drag-to-Applications users
cp -R "$APP" "$STAGE/"

# Loose CLI binary for terminal-only users
cp -f target/release/clipd "$STAGE/"
chmod +x "$STAGE/clipd"

# Include install helper and README
cp -f install.sh "$STAGE/" 2>/dev/null || true
cp -f README.md  "$STAGE/" 2>/dev/null || true

(cd target/release && zip -qr "${ZIP_NAME}.zip" "${ZIP_NAME}/")
rm -rf "$STAGE"

echo ""
echo "════════════════════════════════════════"
echo "  Done!  Artifacts in target/release/:"
echo ""
echo "  Clipd.app                — drag to /Applications"
echo "  ${ZIP_NAME}.zip  — upload to GitHub Releases"
echo ""
echo "  Upload: gh release create ${VERSION} target/release/${ZIP_NAME}.zip"
echo "════════════════════════════════════════"
