#!/usr/bin/env bash
# Manages the canvasd launchd agent (com.piercemakes.canvasd).
#
# admin.toml's [actions.service] and [actions.service-install] call this
# script directly instead of the admin tool's built-in `kind = "python"`
# launchd_service() helper: `admin check` rejects `kind = "python"` in every
# manifest (a bug in the admin tool itself, filed upstream), so canvasd's
# launchd install/status/restart/stop/start is done here with plain
# launchctl instead.
#
# Run from the repo root (admin invokes shell actions with cwd = project
# root), e.g. `./scripts/canvasd-service.sh install`.
set -euo pipefail

LABEL="com.piercemakes.canvasd"
UID_GUI="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/${LABEL}.plist"
PROGRAM="$HOME/.local/bin/canvasd"
SOURCE="target/release/canvasd"
LOG="$HOME/Library/Logs/canvasd.log"

write_plist() {
  mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs" "$(dirname "$PROGRAM")"
  cat >"$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${PROGRAM}</string>
  </array>
  <key>KeepAlive</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${LOG}</string>
  <key>StandardErrorPath</key>
  <string>${LOG}</string>
  <key>WorkingDirectory</key>
  <string>${HOME}</string>
</dict>
</plist>
PLIST_EOF
}

# launchctl bootout returns before launchd has finished removing the service;
# a bootstrap issued in that window fails with "5: Input/output error". Wait
# until the service is gone (up to 10s) before anything re-adds it.
bootout() {
  launchctl bootout "$UID_GUI/$LABEL" 2>/dev/null || true
  for _ in $(seq 1 50); do
    launchctl print "$UID_GUI/$LABEL" >/dev/null 2>&1 || return 0
    sleep 0.2
  done
  echo "canvasd-service: $LABEL still loaded 10s after bootout" >&2
  exit 1
}

install() {
  if [ ! -f "$SOURCE" ]; then
    echo "canvasd-service: $SOURCE not found — run the build step first" >&2
    exit 1
  fi
  mkdir -p "$(dirname "$PROGRAM")"
  cp "$SOURCE" "$PROGRAM"
  write_plist
  bootout
  launchctl bootstrap "$UID_GUI" "$PLIST"
  launchctl enable "$UID_GUI/$LABEL"
  echo "canvasd-service: installed and bootstrapped $LABEL"
}

uninstall() {
  bootout
  rm -f "$PLIST"
  echo "canvasd-service: uninstalled $LABEL"
}

status() {
  launchctl print "$UID_GUI/$LABEL"
}

stop() {
  bootout
  echo "canvasd-service: stopped $LABEL"
}

start() {
  if ! launchctl print "$UID_GUI/$LABEL" >/dev/null 2>&1; then
    launchctl bootstrap "$UID_GUI" "$PLIST"
  fi
  launchctl kickstart -k "$UID_GUI/$LABEL"
  echo "canvasd-service: started $LABEL"
}

restart() {
  launchctl kickstart -k "$UID_GUI/$LABEL"
  echo "canvasd-service: restarted $LABEL"
}

case "${1:-}" in
  install) install ;;
  uninstall) uninstall ;;
  status) status ;;
  stop) stop ;;
  start) start ;;
  restart) restart ;;
  *)
    echo "usage: canvasd-service.sh {install|uninstall|status|start|stop|restart}" >&2
    exit 1
    ;;
esac
