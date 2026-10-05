#!/usr/bin/env bash
# bash scripts/deploy.sh — build and install canvas and Canvas.app from this
# checkout, then install each detected agent's Canvas hooks.
#
# Installing the canvas binary restarts the daemon, which empties the
# in-memory stream. A running Canvas.app is quit before its bundle is
# replaced, and Canvas.app is opened again afterwards (in the background, so
# it never takes focus), whether or not it was running.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
app=/Applications/Canvas.app
exe="$app/Contents/MacOS/app"
canvas="$HOME/.local/bin/canvas"

cd "$repo_root"
cargo build --release -p canvas
admin service install

for agent in claude-code codex; do
  if "$canvas" integrations list --json | grep -q "\"agent\":\"$agent\",\"found\":true"; then
    "$canvas" integrations install "$agent" "$repo_root"
  fi
done

(cd app/src-tauri && cargo tauri build -b app)

if pgrep -f "^$exe" >/dev/null; then
  echo "quitting Canvas.app"
  osascript -e "tell application \"$app\" to quit" >/dev/null
  for _ in $(seq 1 100); do
    pgrep -f "^$exe" >/dev/null || break
    sleep 0.1
  done
  if pgrep -f "^$exe" >/dev/null; then
    echo "Canvas.app did not quit within 10s" >&2
    exit 1
  fi
fi

rm -rf "$app"
cp -R app/src-tauri/target/release/bundle/macos/Canvas.app /Applications/
open -g "$app"
echo "Canvas.app reopened"
