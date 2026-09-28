# Canvas — agent guide

Canvas is one live stream of posts from every Claude Code session on this Mac:
HTML plans, docs and images an agent posts on purpose. It is meant to sit open
beside the terminal all day.

## Map

- `canvasd/` — library crate (axum): the router, state (newest 500 posts, evicted
  oldest-first, held in memory and appended to `stream.jsonl` in
  `CANVAS_DATA_DIR`, default `~/Library/Application Support/canvas`; on start
  the last 24h reload and the file is rewritten compacted), SSE push to
  viewers, and `viewer/` asset serving. No binary of its own — `canvas daemon` runs it.
  `guidance.rs` persists the SessionStart guidance text — an optional global
  override plus a per-repo override map — as `guidance.json` in
  `CANVAS_DATA_DIR`, outside the 24h `stream.jsonl` retention window;
  `AppState::open` loads it alongside the stream reload, so it's async.
- `app/src-tauri/` — Tauri 2 shell: one WKWebView window plus a menu bar icon.
  The window loads a local waiting page (`app/dist/index.html`) that polls
  the daemon and navigates to it once it answers, so a not-yet-started or
  restarting daemon never leaves the window on a dead error page. Its own
  Cargo workspace, outside the root one. It holds no state; quitting it loses
  nothing.
- `viewer/` — plain `index.html` + JS, no build step. A toolbar (search field,
  row of session chips) below the title bar, stream of cards, one per post.
  Session chips and the drawer's per-row eye toggle add/remove sessions from a
  multi-select visible set (everything visible by default) rather than
  picking one session at a time.
- `cli/` — the `canvas` binary: one binary, both roles. `canvas daemon` builds
  `canvasd`'s router and serves it on `127.0.0.1:8229` (`CANVAS_PORT` to
  override), and runs as a launchd agent (`com.piercemakes.canvasd`).
  `canvas hook session-start|session-end` reads Claude Code hook JSON on
  stdin and registers or ends a session; `session-start` always prints the
  guidance block to stdout (Claude Code adds SessionStart stdout to the
  session's context), whether or not registering the session with canvasd
  succeeds — posting only needs a running canvasd, not a registered session
  (`canvas post` creates one server-side if it's missing).
  `canvas guidance` prints that same block unconditionally, for a person or
  agent to read on demand. `canvas post <file|-> [--format md|text|html]`
  reads the session id from `CLAUDE_CODE_SESSION_ID` (Claude Code sets it in
  every Bash tool shell) and creates a new card from Markdown, text or HTML
  read from a file or stdin — format picked by `--format`, else the file
  extension, else Markdown — creating the session if the daemon doesn't
  already know it (e.g. after a daemon restart) — and unlike a hook it fails
  loudly — one line on stderr, non-zero exit — on any error, including when
  `CLAUDE_CODE_SESSION_ID` isn't set. On success it prints one JSON line to
  stdout: `{"card_id": "...", "images": [...], "targets": [...]}`. Hook
  dispatch never starts a tokio runtime; only `canvas daemon` does.
  `canvas post --update <card_id> <file>` sends a PUT to `/api/cards/:id`
  instead, replacing that card's html/images/targets in place (same id and
  session_id) rather than creating a new one; 404s if the id doesn't exist.
  A card can send one value back (canvas-17z): its script posts
  `{type:'canvas-reply', value}` to the viewer, which relays it to
  `POST /api/cards/:id/reply` keyed by which iframe sent it, capped at 4KB,
  last write wins, in memory only. `canvas wait <card_id> [--timeout secs]`
  blocks until a reply lands (default 30s); `canvas replies <card_id>` is
  the non-blocking single check.
- `plugin/` — the Claude Code plugin (`.claude-plugin/marketplace.json` at the
  repo root lists it): `hooks/hooks.json` wires SessionStart and SessionEnd to
  `~/.local/bin/canvas hook …`, `guidance.md` holds the compiled-in default
  guidance text (`include_str!`), and `skills/canvas/` is the skill agents
  load to know how to post. `canvas hook session-start` and `canvas
  guidance` call `GET /api/guidance?cwd=<cwd>` and use, in order, the
  daemon's per-repo override, its global override, then the compiled-in
  default from `guidance.md`. Claude Code runs a cached copy of
  `plugin/`, not the repo: `canvas install [repo]` registers the checkout as a
  local `directory` marketplace (never a git remote — the repo is private) and
  installs or updates `canvas@canvas` from it. `admin deploy canvas` installs
  the `canvas` binary and then runs `canvas install`, so the binary and the
  plugin always come from the same commit. Bump `plugin/.claude-plugin/plugin.json`'s
  `version` when `plugin/` changes, or `claude plugin update` keeps the old copy.

## Rules

- A hook never slows a Claude session: every request to the daemon has a 1s
  timeout and the hook exits 0 on any failure.
- HTML posts render in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`)
  with a CSP that allows scripts only from cdnjs, jsdelivr and unpkg and no
  `connect-src` — a card can still hand one value back via `canvas-reply`
  (canvas-17z), but only by posting it up to the viewer, never by fetching
  anything itself. Never set post markup as `innerHTML` in the viewer's own origin.
- Every `canvas post` creates its own card; nothing creates a card automatically.
- canvasd answers only requests whose `Host` is `127.0.0.1` or `localhost` (any
  port); anything else, or no `Host`, gets 421. That router-wide layer is the
  DNS-rebinding defence: add routes inside `build_router`, never around it, and
  point clients at one of those two names.

## Commands

`admin.toml` is the source of truth; run tasks with `admin <cmd>` (`admin check`
lists them). Issues are tracked in beads (`bd ready`).
