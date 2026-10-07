#!/usr/bin/env bash
# bash scripts/verify-app.sh <start|shot <png>|eval <js|->|stop|env> — run a throwaway canvas daemon
# and a dev Canvas.app on their own Unix socket, and capture the app window.
#
# Canvas has no browser-reachable port, so the app window is the only viewer;
# this is how a change to the viewer, the daemon's API or the app shell is
# looked at. Everything lives under /tmp/canvas-verify (short on purpose: a
# Unix socket path is limited to about 100 characters) and never touches the
# live daemon's socket.
#
#   start       build both, start daemon and app, print the socket path
#   shot <png>  capture the app's "Canvas" window to <png> (no focus taken)
#   eval <js|-> run a script in the app's main window (no focus taken); the
#               debug app reads it from ${dir}/debug/eval within 200ms. A
#               ${dir}/debug/save-path file answers an export's save dialog
#               (its path; empty for cancelled) — see app/src-tauri/src/debug.rs
#   env         print `export CANVAS_SOCKET=...` for driving it with `canvas post`
#   stop        kill exactly the two processes `start` recorded
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${CANVAS_VERIFY_DIR:-/tmp/canvas-verify}"
socket="${dir}/canvasd.sock"

case "${1:-}" in
  start)
    if [ -f "${dir}/app.pid" ] && kill -0 "$(cat "${dir}/app.pid")" 2>/dev/null; then
      echo "verify-app: already running (bash scripts/verify-app.sh stop first)" >&2
      exit 1
    fi
    rm -rf "${dir}"
    mkdir -p "${dir}"
    cargo build -q --manifest-path "${repo_root}/Cargo.toml" -p canvas
    cargo build -q --manifest-path "${repo_root}/app/src-tauri/Cargo.toml"
    export CANVAS_DATA_DIR="${dir}" CANVAS_SOCKET="${socket}"
    "${repo_root}/target/debug/canvas" daemon > "${dir}/daemon.log" 2>&1 &
    echo $! > "${dir}/daemon.pid"
    for _ in $(seq 1 50); do [ -S "${socket}" ] && break; sleep 0.1; done
    [ -S "${socket}" ] || { echo "verify-app: daemon never bound ${socket}" >&2; exit 1; }
    mkdir -p "${dir}/debug/eval"
    CANVAS_DEBUG_DIR="${dir}/debug" "${repo_root}/app/src-tauri/target/debug/app" > "${dir}/app.log" 2>&1 &
    echo $! > "${dir}/app.pid"
    sleep 3
    echo "verify-app: daemon $(cat "${dir}/daemon.pid"), app $(cat "${dir}/app.pid"), socket ${socket}"
    ;;
  shot)
    out="${2:?usage: bash scripts/verify-app.sh shot <png>}"
    pid="$(cat "${dir}/app.pid")"
    cat > "${dir}/winid.swift" <<EOF
import CoreGraphics
import Foundation
let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
for w in list where (w[kCGWindowOwnerPID as String] as? Int) == ${pid} && (w[kCGWindowName as String] as? String) == "Canvas" {
  print(w[kCGWindowNumber as String] ?? 0)
}
EOF
    win="$(swift "${dir}/winid.swift")"
    [ -n "${win}" ] || { echo "verify-app: no Canvas window for pid ${pid}" >&2; exit 1; }
    screencapture -x -o -l "${win}" "${out}"
    echo "verify-app: wrote ${out}"
    ;;
  eval)
    src="${2:?usage: bash scripts/verify-app.sh eval <js|->}"
    tmp="${dir}/debug/eval/.$$.tmp"
    if [ "${src}" = "-" ]; then cat > "${tmp}"; else cp "${src}" "${tmp}"; fi
    mv "${tmp}" "${dir}/debug/eval/$(date +%s%N)-$$.js"
    ;;
  env)
    echo "export CANVAS_SOCKET=${socket}"
    ;;
  stop)
    for name in app daemon; do
      if [ -f "${dir}/${name}.pid" ]; then
        kill "$(cat "${dir}/${name}.pid")" 2>/dev/null || true
        rm -f "${dir}/${name}.pid"
      fi
    done
    echo "verify-app: stopped"
    ;;
  *)
    echo "usage: bash scripts/verify-app.sh <start|shot <png>|eval <js|->|stop|env>" >&2
    exit 2
    ;;
esac
