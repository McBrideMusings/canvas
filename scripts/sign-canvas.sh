#!/usr/bin/env bash
# bash scripts/sign-canvas.sh — sign target/release/canvas with a stable identity.
#
# canvasd runs agents' refresh commands, so macOS privacy prompts name "canvas",
# and macOS ties the person's answer to the binary's signature. A linker or
# ad-hoc signature changes with every build, so each deploy would ask again;
# CANVAS_SIGN_IDENTITY (a codesigning identity's name or SHA-1, from
# `security find-identity -v -p codesigning`) and a fixed identifier keep it the
# same. Both installers (admin service install, Canvas.app's bundled copy) take
# these exact bytes, so the app never sees a mismatch and restarts the daemon.
set -euo pipefail

: "${CANVAS_SIGN_IDENTITY:?set CANVAS_SIGN_IDENTITY to a codesigning identity}"

id=com.piercemakes.canvas.cli
bin="$(cd "$(dirname "$0")/.." && pwd)/target/release/canvas"
# A signature carries its signing time, so re-signing an unchanged binary would
# change its bytes; a fresh link always comes back with the linker's identifier.
if codesign --verify --strict "$bin" 2>/dev/null; then
  details="$(codesign -dv "$bin" 2>&1)"
  if grep -qx "Identifier=$id" <<<"$details"; then
    exit 0
  fi
fi
codesign --force --sign "$CANVAS_SIGN_IDENTITY" --identifier "$id" "$bin"
codesign --verify --strict "$bin"
