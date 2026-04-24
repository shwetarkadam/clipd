#!/usr/bin/env bash
# Build Windows .exe (GNU / mingw target) and a release-style zip from macOS or Linux.
# Requires: rustup target add x86_64-pc-windows-gnu, and mingw linker (see .cargo/config.toml).
#
# Usage:
#   ./dist-windows-cross.sh           # version from workspace Cargo.toml
#   ./dist-windows-cross.sh v0.2.0  # override tag name inside the zip folder
#
# Produces:
#   target/release/Clipd-windows-x86_64-<ver>.zip
#   (folder inside zip matches install.ps1 / GitHub Releases: Clipd-windows-x86_64-<ver>/)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

VERSION="${1:-v$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
ARCH="x86_64"
PKG="Clipd-windows-${ARCH}-${VERSION}"
STAGE="target/release/${PKG}"
GNU="target/x86_64-pc-windows-gnu/release"

echo "╔════════════════════════════════════════════╗"
echo "║  clipd Windows cross-dist — ${VERSION}     "
echo "╚════════════════════════════════════════════╝"
echo ""

echo "==> cargo build --release --target x86_64-pc-windows-gnu"
cargo build --release --target x86_64-pc-windows-gnu

rm -rf "$STAGE" "target/release/${PKG}.zip"
mkdir -p "$STAGE"

for bin in clipd.exe clipd-ui.exe clipd-gui.exe clipd-mcp.exe; do
  if [[ ! -f "$GNU/$bin" ]]; then
    echo "Missing $GNU/$bin"
    exit 1
  fi
  cp -f "$GNU/$bin" "$STAGE/"
done

cp -f install.ps1 README.md "$STAGE/" 2>/dev/null || true
cp -f packaging/windows/Clipd.bat packaging/windows/ClipdTray.vbs "$STAGE/" 2>/dev/null || true
cp -f packaging/windows/ClipdDebug.bat "$STAGE/" 2>/dev/null || true

echo "==> Creating target/release/${PKG}.zip"
( cd target/release && zip -qr "${PKG}.zip" "${PKG}" )
rm -rf "$STAGE"

echo ""
echo "  Done → target/release/${PKG}.zip"
echo "  Upload this zip to GitHub Releases as: ${PKG}.zip (tag ${VERSION})"
echo ""
