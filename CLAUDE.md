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
  `profiles.rs` is the generalized named-profile store a future feature can
  reuse instead of growing its own bespoke override: a "kind" owns its own
  set of named text profiles, which one is assigned globally, which repos
  have their own assignment, and a mode. In `additive` mode (the default) a
  session gets the global profile's text (the built-in default when none is
  assigned) followed by its repo's profile, joined by a blank line and never
  the same profile twice; in `replace` mode it gets the repo's profile alone,
  else the global one. `ProfileSet::compose` returns the joined text and the
  ordered source names (`"built-in"` marks the default). `posting-guidance` is
  the only kind that ships today, and there's no kind switcher in Settings
  until a second one does. Persisted as `profiles.json` in `CANVAS_DATA_DIR`
  (the mode is written only when it isn't `additive`), outside the 24h
  `stream.jsonl` retention window; `AppState::open` loads it alongside the
  stream reload, so it's async.
- `app/src-tauri/` — Tauri 2 shell: one WKWebView window plus a menu bar icon.
  The windows load `canvas://localhost/`, a custom scheme `bridge.rs` answers by
  proxying each request to canvasd's Unix socket (a request that can't reach it
  gets a 503 carrying `app/dist/index.html`, a page that reloads itself, so a
  not-yet-started or restarting daemon never leaves the window on a dead error
  page). A custom scheme response is one whole body, so it can't carry SSE:
  `bridge::forward_events` holds the one `/api/events` stream and re-emits it
  as `canvas-event` and `canvas-stream` Tauri events, which the viewer listens
  for. Tauri counts a custom scheme as a local URL, so
  `capabilities/viewer.json` is `local: true`; a card's sandboxed iframe still
  gets no `window.__TAURI__` and can't invoke any command (probed in the app). Its own
  Cargo workspace, outside the root one. It holds no persisted state;
  quitting it loses nothing. `daemon.rs` makes the app self-installing: on a
  release build's `setup()`, `ensure_daemon` copies the `canvas` binary
  bundled as a Tauri resource (`tauri.conf.json`) out to `~/.local/bin` and
  registers it as the `com.piercemakes.canvasd` launchd agent, skipping the
  copy and restart when the bundled binary already matches what's installed
  so an ordinary relaunch doesn't empty the daemon's in-memory stream. A
  debug build skips this — `admin dev canvas` runs the daemon separately.
  It registers the `canvas-post` URL scheme (`tauri-plugin-deep-link`,
  `tauri.conf.json`; only a bundled build carries it in `Info.plist`): opening
  `canvas-post://<card_id>` shows the window and parks the id in `PendingCard`,
  and the viewer takes it with `take_pending_card` once its state has loaded,
  clears search and filters, scrolls to the card and rings it.
  The `daemon_status` command reports installed/up-to-date/loaded/running
  plus any install error, and the Settings window's Daemon tab polls it.
- `viewer/` — plain `index.html` + JS, no build step, loaded only by
  Canvas.app (there is no browser-reachable port). A toolbar (search field,
  row of session chips) below the title bar, stream of cards, one per post.
  Session chips and the drawer's per-row eye toggle add/remove sessions from a
  multi-select visible set (everything visible by default) rather than
  picking one session at a time.
- `cli/` — the `canvas` binary: one binary, both roles. `canvas daemon` builds
  `canvasd`'s router and serves it on a Unix socket, `canvasd.sock` in
  `CANVAS_DATA_DIR` (`CANVAS_SOCKET` to override), mode 0600 — there is no TCP
  port. The CLI, hooks and app reach it through `canvas-core`'s `unix_http`
  client; `canvas daemon` refuses to start when something already answers on
  the socket. It runs as a launchd agent (`com.piercemakes.canvasd`).
  `canvas hook session-start|session-end` reads Claude Code hook JSON on
  stdin and registers or ends a session; `session-start` always prints the
  guidance block to stdout (Claude Code adds SessionStart stdout to the
  session's context), whether or not registering the session with canvasd
  succeeds — posting only needs a running canvasd, not a registered session
  (`canvas post` creates one server-side if it's missing).
  `canvas guidance` prints that same block unconditionally, for a person or
  agent to read on demand.
  `canvas profile list|show|set|delete|assign|unassign` reads and changes the
  named profiles from the shell, globally or per repo (`--repo owner/name`, or
  `--here` for the current directory's GitHub repo, resolved by canvasd's own
  `repo::github_repo_blocking`), failing loudly like `canvas post`.
  `canvas post <file|-> [--format md|text|html]`
  reads the session id from `CLAUDE_CODE_SESSION_ID` (Claude Code sets it in
  every Bash tool shell) and creates a new card from Markdown, text or HTML
  read from a file or stdin — format picked by `--format`, else the file
  extension, else Markdown — creating the session if the daemon doesn't
  already know it (e.g. after a daemon restart) — and unlike a hook it fails
  loudly — one line on stderr, non-zero exit — on any error, including when
  `CLAUDE_CODE_SESSION_ID` isn't set. On success it prints one JSON line to
  stdout: `{"card_id": "...", "images": [...], "targets": [...]}`. Hook
  dispatch never starts a tokio runtime; only `canvas daemon` does.
  `canvas card <card_id>` prints one post as JSON (`GET /api/cards/:id`), so
  a `canvas-post://<card_id>` link from a post's "Copy post link" menu item
  can be read by an agent.
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
  guidance` call `GET /api/profiles/posting-guidance/effective?cwd=<cwd>`, which
  returns the text already joined per the kind's mode plus the ordered `profiles`
  it came from (`GET /api/profiles/:kind` is the settings page's full read; `PUT
  /api/profiles/:kind/mode` sets the mode), and print that text; when nothing is
  assigned the response has no text and they print the compiled-in default from
  `guidance.md`. Claude Code runs a cached copy of
  `plugin/`, not the repo: `canvas install [repo]` registers the checkout as a
  local `directory` marketplace (never a git remote, so the plugin comes from the same checkout as the binary) and
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
- canvasd has no TCP listener, so nothing reaches it except a process that can
  open its 0600 socket, and its routes carry no Host or Origin checks. Never add
  a TCP or other network listener to it; a client that needs it goes through the
  socket (Canvas.app's `canvas://` proxy is the only bridge for a webview).

## Commands

`admin.toml` is the source of truth; run tasks with `admin <cmd>` (`admin check`
lists them). Issues are tracked in beads (`bd ready`).
