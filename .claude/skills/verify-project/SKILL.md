---
name: verify-project
description: Verify canvasd (the Rust/axum daemon at 127.0.0.1:8229) and its plain-HTML viewer actually stream and render turn cards. Use before closing any Canvas ticket that touches canvasd's HTTP API, the ring buffer, SSE, or viewer/.
---

# Verify Canvas

Canvas is a background daemon (`canvasd`, Rust/axum) holding sessions and a ring of the
newest 500 turn cards in memory, plus a plain-HTML/JS viewer it serves at `/`. There is
no database and no build step for the viewer — the whole surface is one running process
and one browser tab.

## Launch

Build once, then run canvasd on a spare port so you never collide with a real instance
on 8229 (its default) or 8231 (this skill's convention):

```
cargo build --manifest-path <repo>/Cargo.toml -p canvasd
CANVAS_PORT=8231 <repo>/target/debug/canvasd &
```

Readiness: `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8231/` returns `200`.

## Doctor

Before driving anything, confirm the instance you're about to hit is actually yours and
alive:

```
ps aux | grep '[t]arget/debug/canvasd'
curl -s http://127.0.0.1:8231/api/state
```

`api/state` on a freshly launched instance returns `{"sessions":[],"cards":[]}`. If it
doesn't, you're pointed at someone else's instance — stop and use a different port.

## Drive

The API is the primary surface; the viewer renders whatever the API holds. Drive both.

**API, with curl** (see `features/session-and-turn-stream.md` and
`features/explicit-posts.md` for the full request/response shapes):

```
curl -s -X POST http://127.0.0.1:8231/api/sessions \
  -H 'content-type: application/json' \
  -d '{"session_id":"s1","cwd":"/Users/me/Projects/canvas","claude_pid":11111}'
```

**Viewer, with Playwright** (headless Chrome via `--headless --screenshot` has hung in
this repo — the SSE connection at `/api/events` never closes, so Chrome's own
`--screenshot` mode waits forever for network idle. Use the `playwright` MCP tools
instead, which screenshot on demand without waiting for idle):

```
browser_navigate to http://127.0.0.1:8231/
browser_take_screenshot (fullPage: true)
```

## Evidence

For each of the three viewer states, a screenshot a human can look at:

- **Empty** — fresh instance, no seed data. Copy must read exactly: "No posts yet.
  Canvas shows links, files and images from your Claude sessions as they work."
- **Populated** — after seeding two sessions, a turn (link + path + image), and an
  explicit HTML post. Compare layout feel (sidebar + card column, card header row)
  against `docs/spikes/sideshow-reference/populated.png` if that directory still exists
  — later tickets delete it.
- **Disconnected** — kill canvasd out from under an open viewer tab, wait ~2s. Banner
  must read exactly: "Reconnecting to canvasd…"

Also run the API test suite and paste the pass count:

```
cargo test --workspace --manifest-path <repo>/Cargo.toml
```

## Hooks CLI (`cli/`, the `canvas` binary)

Build with `cargo build -p canvas`. Point it at your spare-port canvasd with `CANVAS_URL`:

```
echo '{"session_id":"t1","cwd":"/Users/me/Projects/canvas"}' | CANVAS_URL=http://127.0.0.1:8231 target/debug/canvas hook session-start
echo '{"session_id":"t1","cwd":"/Users/me/Projects/canvas","transcript_path":"<real ~/.claude/projects/*/*.jsonl>"}' | CANVAS_URL=http://127.0.0.1:8231 target/debug/canvas hook stop
```

Pass: `/api/state` gains a card with that transcript's last-turn links/paths/images.
Loop over the 15 newest transcripts — some end in an empty turn and correctly produce
no card. Silence check: with `CANVAS_URL=http://10.255.255.1:8229` (never answers)
each hook exits 0, prints nothing, in about 1.0s.

The context-mode hook blocks a bare `curl` Bash call; inside a `bash <script>` it runs,
or use `python3 -c` with `urllib.request`.

## Canvas.app and the launchd agent

`admin deploy` from the main checkout installs `~/.local/bin/canvasd` under launchd
(`com.piercemakes.canvasd`) and copies `/Applications/Canvas.app`. It runs the real
agent on 8229, so seed it with python and clear it afterwards with
`launchctl kickstart -k gui/$(id -u)/com.piercemakes.canvasd`.

- Service: `launchctl print gui/$(id -u)/com.piercemakes.canvasd | grep -E '^\s*(state|pid) ='`
  shows `state = running`. `kill -9` its pid; within 2s a new pid serves 200 on 8229.
- Window: `open -g /Applications/Canvas.app` (no focus steal). Find the window id with
  a `CGWindowListCopyWindowInfo` swift script — take the one whose `kCGWindowName` is
  `Canvas` (a 500x500 unnamed window from the same app captures blank white). Then
  `screencapture -x -o -l <id> <png>` and look at it.
- Quit: `osascript -e 'tell application id "com.piercemakes.canvas" to quit'`.
- Not drivable from here: the tray menu (Show/Hide, Quit) and close-hides-window. Those
  need a person clicking.

## Cleanup

Kill the exact PID you started, never `pkill -f canvasd` blindly (a real launchd-managed
canvasd may be running on 8229 at the same time):

```
kill <pid-from-launch>
```

Confirm it's gone: `curl` to the port now fails to connect.

## Helpers

None shipped yet. If a seed script accumulates enough curl calls to be worth keeping,
add it here as `scripts/seed.sh` and document its invocation.

## Not verification

- `cargo build` succeeding says nothing about the ring buffer, SSE, or the open/close
  card lifecycle — those need the API driven live.
- A green `cargo test --workspace` run is not a screenshot. The acceptance criteria for
  any viewer-facing change require looking at the rendered page.
- Reading `viewer/app.js` and reasoning about what it should do is not the same as
  loading it in a browser — CSP mistakes, iframe sandbox mistakes, and postMessage
  bridge bugs only show up live.
