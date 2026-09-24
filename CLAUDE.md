# Canvas — agent guide

Canvas is one live stream of posts from every Claude Code session on this Mac:
links, file paths, images, and HTML plans or docs. It is meant to sit open beside
the terminal all day.

## Map

- `canvasd/` — library crate (axum): the router, state (newest 500 posts, evicted
  oldest-first, held in memory and appended to `stream.jsonl` in
  `CANVAS_DATA_DIR`, default `~/Library/Application Support/canvas`; on start
  the last 24h reload and the file is rewritten compacted), SSE push to
  viewers, and `viewer/` asset serving. No binary of its own — `canvas daemon` runs it.
- `app/src-tauri/` — Tauri 2 shell: one WKWebView window plus a menu bar icon.
  The window loads a local waiting page (`app/dist/index.html`) that polls
  the daemon and navigates to it once it answers, so a not-yet-started or
  restarting daemon never leaves the window on a dead error page. Its own
  Cargo workspace, outside the root one. It holds no state; quitting it loses
  nothing.
- `viewer/` — plain `index.html` + JS, no build step. Sidebar of sessions
  ("All" by default), stream of turn cards.
- `cli/` — the `canvas` binary: one binary, both roles. `canvas daemon` builds
  `canvasd`'s router and serves it on `127.0.0.1:8229` (`CANVAS_PORT` to
  override), and runs as a launchd agent (`com.piercemakes.canvasd`).
  `canvas hook session-start|session-end|stop` reads Claude Code hook JSON on
  stdin: SessionStart and SessionEnd register sessions, and Stop turns each
  finished turn into a card. `canvas post <file|->` reads the session id from
  `CLAUDE_CODE_SESSION_ID` (Claude Code sets it in every Bash tool shell) and
  posts HTML from a file or stdin into that session's open card, creating the
  session if the daemon doesn't already know it (e.g. after a daemon
  restart) — and unlike a hook it fails loudly — one line on stderr, non-zero
  exit — on any error, including when `CLAUDE_CODE_SESSION_ID` isn't set.
  Hook dispatch never starts a tokio runtime; only `canvas daemon` does.
- `plugin/` — the Claude Code plugin (`.claude-plugin/marketplace.json` at the
  repo root lists it): `hooks/hooks.json` wires SessionStart, SessionEnd and
  Stop to `~/.local/bin/canvas hook …`, and `skills/canvas/` is the skill agents
  load to know how to post. `admin deploy` installs the `canvas` binary there.

## Rules

- A hook never slows a Claude session: every request to the daemon has a 1s
  timeout and the hook exits 0 on any failure.
- HTML posts render in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`)
  with a CSP that allows scripts only from cdnjs, jsdelivr and unpkg and no
  `connect-src`. Never set post markup as `innerHTML` in the viewer's own origin.
- A turn with no links, paths or images, and no explicit post, produces no card.

## Commands

`admin.toml` is the source of truth; run tasks with `admin <cmd>` (`admin check`
lists them). Issues are tracked in beads (`bd ready`).
