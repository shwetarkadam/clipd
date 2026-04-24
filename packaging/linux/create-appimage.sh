#!/usr/bin/env bash
# Create a Linux AppImage for clipd.
# Requires: appimagetool in PATH (downloaded automatically if missing).
#
# Usage: ./create-appimage.sh [version]
# Produces: Clipd-<version>-x86_64.AppImage
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) APPIMAGE_ARCH="x86_64" ;;
  aarch64|arm64) APPIMAGE_ARCH="aarch64" ;;
  *) echo "Unsupported: $ARCH"; exit 1 ;;
esac

APPDIR="target/release/Clipd.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps"

for bin in clipd clipd-ui clipd-gui clipd-mcp; do
  if [ -f "target/release/${bin}" ]; then
    cp -f "target/release/${bin}" "$APPDIR/usr/bin/"
    chmod +x "$APPDIR/usr/bin/${bin}"
  fi
done

cp -f packaging/linux/clipd.desktop "$APPDIR/clipd.desktop"
cp -f packaging/linux/clipd.desktop "$APPDIR/usr/share/applications/clipd.desktop"
cp -f packaging/icons/clipd.svg "$APPDIR/clipd.svg"
cp -f packaging/icons/clipd.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/clipd.svg"

cat > "$APPDIR/AppRun" << 'APPRUN'
#!/usr/bin/env bash
SELF="$(readlink -f "$0" 2>/dev/null || realpath "$0")"
APPDIR="$(dirname "$SELF")"
export PATH="${APPDIR}/usr/bin:${PATH}"
export XDG_DATA_DIRS="${APPDIR}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "${APPDIR}/usr/bin/clipd-ui" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

echo "[Desktop Entry]" > "$APPDIR/.DirIcon_info"
cp -f packaging/icons/clipd.svg "$APPDIR/.DirIcon"

OUTPUT="Clipd-${VERSION}-${APPIMAGE_ARCH}.AppImage"

if ! command -v appimagetool &>/dev/null; then
  echo "Downloading appimagetool..."
  TOOL_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-${APPIMAGE_ARCH}.AppImage"
  curl -fSL "$TOOL_URL" -o /tmp/appimagetool
  chmod +x /tmp/appimagetool
  APPIMAGETOOL="/tmp/appimagetool"
else
  APPIMAGETOOL="appimagetool"
fi

echo "==> Creating $OUTPUT"
"$APPIMAGETOOL" --no-sandbox "$APPDIR" "target/release/$OUTPUT" 2>/dev/null || \
  "$APPIMAGETOOL" "$APPDIR" "target/release/$OUTPUT"

rm -rf "$APPDIR"
echo "Done: target/release/$OUTPUT"
