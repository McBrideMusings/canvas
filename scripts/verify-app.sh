#!/usr/bin/env bash
# bash scripts/verify-app.sh <start|shot <png> [settings]|eval <js|-> [settings]|env|dir|stop> — run a throwaway canvas daemon
# and a dev Canvas.app on their own Unix socket, and capture the app window.
#
# Canvas has no browser-reachable port, so the app window is the only viewer;
# this is how a change to the viewer, the daemon's API or the app shell is
# looked at. Everything lives under /tmp/cv-<8 hex digits hashed from this
# checkout's path>, or CANVAS_VERIFY_DIR when set, so each checkout runs its own
# daemon and app and two checkouts can verify at once. The path stays short on
# purpose (a Unix socket path is limited to about 100 characters) and never
# touches the live daemon's socket. The dev app's localStorage (the theme, the
# page choice) lives under ~/Library/WebKit/app and is shared by every checkout.
#
#   start       build both, copy them to ${dir}/bin, start daemon and app from
#               there, print the folder and socket. The copy keeps what runs
#               this checkout's build when CARGO_TARGET_DIR is a target folder
#               other checkouts build into too (a swarm's workers share one).
#   shot <png> [settings]
#               capture the app's "Canvas" window, or its "Settings" window, to
#               <png> (no focus taken)
#   eval <js|-> [settings]
#               run a script in the app's main window, or its Settings window
#               (no focus taken; open it first with an eval in the main window
#               that invokes `open_settings`); the
#               debug app reads it from ${dir}/debug/eval within 200ms. A
#               ${dir}/debug/save-path file answers an export's save dialog
#               (its path; empty for cancelled) — see app/src-tauri/src/debug.rs
#   env         print `export CANVAS_DATA_DIR=... CANVAS_SOCKET=... PATH=${dir}/bin:...`
#               and nothing else, for `eval "$(admin verify-app env)"` before
#               `canvas post`, so `canvas` is the copy `start` runs
#   dir         print the folder
#   stop        kill exactly the two processes `start` recorded
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${CANVAS_VERIFY_DIR:-/tmp/cv-$(printf %s "${repo_root}" | shasum | cut -c1-8)}"
socket="${dir}/canvasd.sock"

case "${1:-}" in
  start)
    if [ -f "${dir}/app.pid" ] && kill -0 "$(cat "${dir}/app.pid")" 2>/dev/null; then
      echo "verify-app: already running (bash scripts/verify-app.sh stop first)" >&2
      exit 1
    fi
    rm -rf "${dir}"
    # Not -p: fail if anything recreated the folder after the rm, and keep it private.
    mkdir -m 700 "${dir}"
    cargo build -q --manifest-path "${repo_root}/Cargo.toml" -p canvas
    cargo build -q --manifest-path "${repo_root}/app/src-tauri/Cargo.toml"
    mkdir "${dir}/bin"
    cp "${CARGO_TARGET_DIR:-${repo_root}/target}/debug/canvas" "${dir}/bin/canvas"
    cp "${CARGO_TARGET_DIR:-${repo_root}/app/src-tauri/target}/debug/app" "${dir}/bin/app"
    export CANVAS_DATA_DIR="${dir}" CANVAS_SOCKET="${socket}"
    "${dir}/bin/canvas" daemon > "${dir}/daemon.log" 2>&1 &
    echo $! > "${dir}/daemon.pid"
    for _ in $(seq 1 50); do [ -S "${socket}" ] && break; sleep 0.1; done
    [ -S "${socket}" ] || { echo "verify-app: daemon never bound ${socket}" >&2; exit 1; }
    mkdir -p "${dir}/debug/eval"
    CANVAS_DEBUG_DIR="${dir}/debug" "${dir}/bin/app" > "${dir}/app.log" 2>&1 &
    echo $! > "${dir}/app.pid"
    sleep 3
    echo "verify-app: daemon $(cat "${dir}/daemon.pid"), app $(cat "${dir}/app.pid"), dir ${dir}, socket ${socket}"
    ;;
  shot)
    out="${2:?usage: bash scripts/verify-app.sh shot <png> [settings]}"
    title="Canvas"
    [ "${3:-}" = "settings" ] && title="Settings"
    pid="$(cat "${dir}/app.pid")"
    cat > "${dir}/winid.swift" <<EOF
import CoreGraphics
import Foundation
let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
for w in list where (w[kCGWindowOwnerPID as String] as? Int) == ${pid} && (w[kCGWindowName as String] as? String) == "${title}" {
  print(w[kCGWindowNumber as String] ?? 0)
}
EOF
    win="$(swift "${dir}/winid.swift")"
    [ -n "${win}" ] || { echo "verify-app: no ${title} window for pid ${pid}" >&2; exit 1; }
    screencapture -x -o -l "${win}" "${out}"
    echo "verify-app: wrote ${out}"
    ;;
  eval)
    src="${2:?usage: bash scripts/verify-app.sh eval <js|-> [settings]}"
    suffix="js"
    [ "${3:-}" = "settings" ] && suffix="settings.js"
    tmp="${dir}/debug/eval/.$$.tmp"
    if [ "${src}" = "-" ]; then cat > "${tmp}"; else cp "${src}" "${tmp}"; fi
    mv "${tmp}" "${dir}/debug/eval/$(date +%s%N)-$$.${suffix}"
    ;;
  env)
    printf 'export CANVAS_DATA_DIR=%q CANVAS_SOCKET=%q PATH=%q\n' "${dir}" "${socket}" "${dir}/bin:${PATH}"
    ;;
  dir)
    echo "${dir}"
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
    echo "usage: bash scripts/verify-app.sh <start|shot <png> [settings]|eval <js|-> [settings]|env|dir|stop>" >&2
    exit 2
    ;;
esac
