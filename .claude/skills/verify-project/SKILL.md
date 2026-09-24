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
  -d '{"sessionId":"s1","cwd":"/Users/me/Projects/canvas","claudePid":11111}'
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
