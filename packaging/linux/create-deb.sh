#!/usr/bin/env bash
# Create a .deb package for clipd.
#
# Usage: ./create-deb.sh [version]
# Produces: target/release/clipd_<version>_amd64.deb
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

VERSION="${1:-$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')}"
DEB_VERSION="${VERSION#v}"
ARCH="$(dpkg --print-architecture 2>/dev/null || echo 'amd64')"

DEBDIR="target/release/clipd_${DEB_VERSION}_${ARCH}"
rm -rf "$DEBDIR"
mkdir -p "$DEBDIR/DEBIAN"
mkdir -p "$DEBDIR/usr/bin"
mkdir -p "$DEBDIR/usr/share/applications"
mkdir -p "$DEBDIR/usr/share/icons/hicolor/scalable/apps"

for bin in clipd clipd-ui clipd-gui clipd-mcp; do
  if [ -f "target/release/${bin}" ]; then
    cp -f "target/release/${bin}" "$DEBDIR/usr/bin/"
    chmod 755 "$DEBDIR/usr/bin/${bin}"
  fi
done

cp -f packaging/linux/clipd.desktop "$DEBDIR/usr/share/applications/"
cp -f packaging/icons/clipd.svg "$DEBDIR/usr/share/icons/hicolor/scalable/apps/"

cat > "$DEBDIR/DEBIAN/control" << EOF
Package: clipd
Version: ${DEB_VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Depends: libgtk-3-0, libayatana-appindicator3-1, libdbus-1-3, libxdo3
Maintainer: Shweta Kadam <shweta@clipd.dev>
Description: AI-powered multi-slot clipboard manager
 Multi-slot clipboard with searchable history, smart paste
 transforms, fuzzy and semantic search, and MCP server for
 AI editor integration.
Homepage: https://github.com/shwetarkadam/clipd
License: BSL-1.1
EOF

cat > "$DEBDIR/DEBIAN/postinst" << 'EOF'
#!/bin/bash
set -e
update-desktop-database /usr/share/applications 2>/dev/null || true
gtk-update-icon-cache /usr/share/icons/hicolor 2>/dev/null || true
EOF
chmod 755 "$DEBDIR/DEBIAN/postinst"

dpkg-deb --build "$DEBDIR" "target/release/clipd_${DEB_VERSION}_${ARCH}.deb"
rm -rf "$DEBDIR"
echo "Done: target/release/clipd_${DEB_VERSION}_${ARCH}.deb"
