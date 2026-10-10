#!/usr/bin/env bash
# The code-signing identity clipd builds are signed with, shared by
# create-app-bundle.sh and install-dev.sh.
#
# macOS keys the Accessibility and Input Monitoring grants to the app's
# designated requirement. An ad-hoc signature ("-") pins it to the exact build
# hash, so every rebuild is a new app to macOS and the grant silently stops
# applying — multi-slot copy goes deaf. A real identity (Developer ID, Apple
# Development, or a self-signed "clipd-codesign") keeps the requirement the
# same across builds, so the grant is given once and sticks.
#
# CLIPD_SIGN_ID wins when set ("-" forces ad-hoc).

pick_signing_identity() {
  local list
  list="$(security find-identity -v -p codesigning 2>/dev/null)" || return 0
  local preference
  # Developer ID first (also valid for distribution), then a dev cert, then any.
  for preference in 'Developer ID Application' 'Apple Development' ''; do
    local found
    found="$(printf '%s\n' "$list" \
      | grep -F "\"${preference}" \
      | head -1 \
      | sed -E 's/^[^"]*"(.*)".*$/\1/')"
    if [[ -n "$found" ]]; then
      printf '%s' "$found"
      return 0
    fi
  done
}

clipd_signing_identity() {
  if [[ -n "${CLIPD_SIGN_ID+isset}" ]]; then
    printf '%s' "$CLIPD_SIGN_ID"
    return 0
  fi
  local found
  found="$(pick_signing_identity)"
  printf '%s' "${found:--}"
}
