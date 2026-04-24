#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

mkdir -p "${HOME}/.local/share/applications"
mkdir -p "${HOME}/.local/share/icons"
mkdir -p "${HOME}/.local/bin"

for bin in clipd clipd-ui clipd-gui clipd-mcp; do
  if [ -f "${SCRIPT_DIR}/${bin}" ]; then
    cp -f "${SCRIPT_DIR}/${bin}" "${HOME}/.local/bin/"
    chmod +x "${HOME}/.local/bin/${bin}"
  fi
done

if [ -f "${SCRIPT_DIR}/clipd.svg" ]; then
  cp -f "${SCRIPT_DIR}/clipd.svg" "${HOME}/.local/share/icons/clipd.svg"
fi

sed "s|Exec=clipd-ui|Exec=${HOME}/.local/bin/clipd-ui|" \
  "${SCRIPT_DIR}/clipd.desktop" > "${HOME}/.local/share/applications/clipd.desktop"

update-desktop-database "${HOME}/.local/share/applications" 2>/dev/null || true

echo "Clipd installed. Start from menu or run: clipd-ui"
