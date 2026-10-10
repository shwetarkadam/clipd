#!/usr/bin/env bash
# Build clipd and install it over ~/Applications/Clipd.app, signed with a
# stable identity so the Accessibility / Input Monitoring grant survives the
# install (see signing-identity.sh). For development; releases use release.yml.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=signing-identity.sh
source packaging/macos/signing-identity.sh

APP="${CLIPD_APP:-$HOME/Applications/Clipd.app}"
[[ -d "$APP" ]] || { echo "No app at $APP — install a release first." >&2; exit 1; }

cargo build --release
SIGN_ID="$(clipd_signing_identity)"
if [[ "$SIGN_ID" == "-" ]]; then
  echo "==> ad-hoc signing: the Accessibility grant will NOT survive this install." >&2
else
  echo "==> signing with: $SIGN_ID"
fi

osascript -e 'quit app "Clipd"' 2>/dev/null || true
sleep 2
pkill -x clipd-ui 2>/dev/null || true
pkill -x clipd-gui 2>/dev/null || true
sleep 1

for bin in clipd clipd-gui clipd-ui; do
  cp -f "target/release/$bin" "$APP/Contents/MacOS/$bin"
done
for bin in clipd clipd-gui clipd-ui clipd-hud clipd-ocr; do
  [[ -f "$APP/Contents/MacOS/$bin" ]] || continue
  codesign --force --sign "$SIGN_ID" "$APP/Contents/MacOS/$bin"
done
codesign --force --deep --sign "$SIGN_ID" "$APP"
codesign -dr - "$APP" 2>&1 | tail -1
open "$APP"
echo "==> installed and relaunched $APP"
