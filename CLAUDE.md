# Canvas — agent guide

Canvas is one live stream of posts from every Claude Code session on this Mac:
HTML plans, docs and images an agent posts on purpose. It is meant to sit open
beside the terminal all day.

## Map

- `canvasd/` — library crate (axum): the router, state (newest 500 posts, evicted
  oldest-first, held in memory and appended to `stream.jsonl` in
  `CANVAS_DATA_DIR`, default `~/Library/Application Support/canvas`; on start
  the last 24h reload, dropping any session with no post left, and the file is rewritten compacted; each session
  records the `agent` that started it — `claude-code` and `codex` are the values —
  required on `/api/posts`, kept from the first post, and a stored session without one reloads as Claude Code;
  it also records the agent's `pid` from its first post (`CLAUDE_PID` for Claude
  Code, the first `codex` ancestor for Codex), and a 30s sweep ends a session whose
  pid is gone, releasing its session-scoped pins; sessions with no pid are never
  swept), SSE push to
  viewers, and `viewer/` asset serving. No binary of its own — `canvas daemon` runs it.
  `profiles.rs` is the generalized named-profile store a future feature can
  reuse instead of growing its own bespoke override: a "kind" owns its own
  set of named text profiles, which one is assigned globally, which repos
  have their own assignment, and a mode. In `additive` mode (the default) a
  session gets the global profile's text (the built-in default when none is
  assigned) followed by its repo's profile, joined by a blank line and never
  the same profile twice; in `replace` mode it gets the repo's profile alone,
  else the global one. `ProfileSet::compose` returns the joined text and the
  ordered source names (`"built-in"` marks the default). Two kinds ship:
  `posting-guidance` (text an agent reads) and `stop-triggers` (directives
  the prompt hook parses; `stop_triggers.rs` holds the parser, and the daemon
  answers 400 with the offending line for a `stop-triggers` profile that
  doesn't parse). Settings' Guidance tab switches between them. Persisted as `profiles.json` in `CANVAS_DATA_DIR`
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
  Its `relaunchNeeded` is true only when a bundled binary exists and differs from
  the installed one (never in a debug build); then the main viewer shows a
  dismissible banner (dismissed until the next launch), Settings > Daemon a
  Relaunch button beside "Matches this app", and Settings > Integrations a
  notice, each calling the `relaunch` command (`AppHandle::restart`, so the
  next launch's `setup()` reinstalls the daemon and syncs integrations;
  relaunching restarts the daemon and empties the in-memory stream).
  `integrations.rs` keeps each agent's hooks current by running the installed
  `canvas integrations list --json` and `install <agent>` (`CANVAS_BIN`
  names a stand-in binary in a debug build; the subprocess PATH gains `~/.local/bin`,
  `/opt/homebrew/bin` and `/usr/local/bin`, since a Finder-launched app has
  none of them): a release build's `setup()` installs every detected agent
  that is out of date or not installed, on a background thread after the
  daemon step, and a failed install's stderr line stays in `IntegrationState`
  until that agent's next success. The `integration_status` and
  `integration_install(agent)` commands (agent from the fixed list
  `claude-code`, `codex`) feed the Settings window's Integrations tab, which
  shows a row per agent with a Retry, Update or Install button.
- `viewer/` — plain `index.html` + JS, no build step, loaded only by
  Canvas.app (there is no browser-reachable port). A toolbar (search field,
  row of session chips) below the title bar, stream of cards, one per post.
  Session chips and the drawer's per-row eye toggle add/remove sessions from a
  multi-select visible set (everything visible by default) rather than
  picking one session at a time.
  A theme button between Pin and Sessions switches light and dark for the
  whole window and every card (`theme.js` holds the choice in localStorage,
  `theme.css` the palette; with nothing stored the system decides). A card
  always renders in the active theme: `buildIframeDoc` rewrites its
  `prefers-color-scheme` queries, forces the page surface and ink, and recolors
  text that falls below 4.5:1 contrast; a change rebuilds every card iframe.
  A card image opens in a lightbox (`openLightbox` in `app.js`): a click on the
  image toggles fit and 2.5x, wheel or pinch zooms at the cursor (1x to 8x),
  drag pans while zoomed, `+` `-` `0` zoom and reset, and only the X button,
  Esc or a click on the bare backdrop closes it.
  A pinned card (`card.pin`) never enters `#cards`: `app.js` puts it on the pin
  shelf, a `.shelf` section under the toolbar that exists only while something is
  pinned (label `Pinned · N`, widgets in slot order). A widget is the pin's
  `widgetHtml` in a sandboxed iframe built like a card's, else the card's first
  heading (else its first text line, else the slot), under one transparent button; a widget is replaced one at a time and
  never moved (moving an iframe reloads it). Past the row's width the extra
  widgets go inert behind an edge fade and a `+N` button that wraps the row.
  A click opens the card as a `.sheet` dialog over the dimmed feed (Escape,
  the scrim or Close dismisses it and focus returns to the widget); a pin's
  `refreshError` shows as a `.w-error` mark and a `.sheet-error` line. A card
  gaining or losing `pin` through `card-upserted` moves between feed and shelf.
  Settings > Guidance > Post reminders edits each profile as a form
  (`stop-form.js` parses the directive text into a model and re-emits it, keeping
  comments, unknown lines and order) or as raw text; the text stays the only store.
- `cli/` — the `canvas` binary: one binary, both roles. `canvas daemon` builds
  `canvasd`'s router and serves it on a Unix socket, `canvasd.sock` in
  `CANVAS_DATA_DIR` (`CANVAS_SOCKET` to override), mode 0600 — there is no TCP
  port. The CLI, hooks and app reach it through `canvas-core`'s `unix_http`
  client; `canvas daemon` refuses to start when something already answers on
  the socket. It runs as a launchd agent (`com.piercemakes.canvasd`).
  `canvas hook session-start|session-end` reads Claude Code hook JSON on
  stdin and ends a session (a no-op for one that never posted); `session-start`
  registers nothing and always prints the guidance block to stdout (Claude Code
  adds SessionStart stdout to the session's context). A session exists in
  canvasd only once its first `canvas post` creates it, so one that never posts
  never appears in Canvas; there is no session-registration route.
  `canvas hook prompt` (the plugin's UserPromptSubmit hook, `cli/src/stop.rs`)
  reads the transcript's last finished turn and, when a trigger the `stop-triggers` profile in
  effect for the session's cwd enables fired (an image looked at, a file
  changed outside scratch space, a long block, several links, `report`,
  `verify`; the default enables only the image trigger) and no `canvas post` ran, prints a
  one-line reminder that Claude Code adds to the new prompt's context (no block,
  no error label, no re-sent reply; the post comes a turn late); otherwise it
  prints nothing. With nothing assigned or canvasd unreachable it uses
  `plugin/stop-triggers.txt`, compiled in.
  Everything that differs between coding agents sits behind the `AgentAdapter`
  trait in `cli/src/agent.rs`: the environment variable that names the session
  (`CLAUDE_CODE_SESSION_ID` for Claude Code, `CODEX_THREAD_ID` for Codex; `canvas
  post` reads whichever is set), the hook's stdin shape, and how the transcript
  (Claude Code's JSONL, Codex's rollout) reads as a finished turn. Codex reads text
  files through the shell, so its turns list only images (`view_image`) as read paths. `canvas hook
  <event> --agent <name>` picks the adapter; with no flag it is Claude Code, and
  an unknown name makes the hook exit 0 silently.
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
  `canvas data <card_id> [file|-]` pushes one JSON value (up to 256KB) into a
  running card: `PUT /api/cards/:id/data` keeps the latest (last write wins, in
  memory, dropped with its card) and publishes a `card-data` SSE event, which
  the viewer posts into that card's iframe as `{type:'canvas-data', value}`
  without rebuilding it, and again after any iframe load. `--update` rebuilds
  the iframe (`upsertCard`), so a dashboard posts its HTML once and streams
  values with `canvas data`. The value is never written to `stream.jsonl`.
- Pinned posts: `canvas post --pin <slot> [--pin-scope session|repo] [--widget <file>]`
  holds the card in a slot, unique within the poster's repo (its cwd when it has
  none); a later post to a held slot replaces that card in place, keeping its id,
  and takes over its session. A session-scoped pin returns to the feed when its
  session ends; a repo-scoped pin survives session end, session delete and clearing
  a session's cards. `canvas unpin <slot|card_id>` clears `pin` and the card
  returns to the feed at its own `at`; `canvas data --slot <slot>` pushes to the
  card in the slot (`GET /api/pins?cwd=&slot=` resolves it; `DELETE
  /api/cards/:id/pin` unpins). A pinned card is never evicted from the ring or
  pruned by age, but counts toward the 500. The window's always-on-top toggle is
  "keep on top" (`get_keep_on_top`/`set_keep_on_top`), not a pin.
- `canvas integrations list [--json]|install <agent> [repo]` (`cli/src/integrations/`)
  detects each agent (its CLI on PATH), reports `current`, `out of date`,
  `not installed` or `needs review`, and installs; per agent it is
  `AgentAdapter::{detect, status, install}`. Codex's install merges Canvas's
  SessionStart, UserPromptSubmit and SessionEnd groups into
  `~/.codex/hooks.json` (`$CODEX_HOME`), replacing only its own groups and
  appending new ones so other tools' entries keep their positions, and writes
  `canvas-hooks-version` beside it. Codex runs a hook only after the user
  trusts it (the TUI's "Hooks need review"; `trusted_hash` tables in
  `config.toml`, computed by Codex), so status is `needs review` until those
  tables exist for Canvas's groups.
- `plugin/` — the Claude Code plugin (`.claude-plugin/marketplace.json` at the
  repo root lists it): `hooks/hooks.json` wires SessionStart, SessionEnd and UserPromptSubmit to
  `~/.local/bin/canvas hook …`, `guidance.md` holds the compiled-in default
  guidance text (`include_str!`), and `skills/canvas/` is the skill agents
  load to know how to post. `canvas hook session-start` and `canvas
  guidance` call `GET /api/profiles/posting-guidance/effective?cwd=<cwd>`, which
  returns the text already joined per the kind's mode plus the ordered `profiles`
  it came from (`GET /api/profiles/:kind` is the settings page's full read; `PUT
  /api/profiles/:kind/mode` sets the mode), and print that text; when nothing is
  assigned the response has no text and they print the compiled-in default from
  `guidance.md`. Claude Code runs a cached copy of
  `plugin/`, not the repo: `canvas integrations install claude-code [repo]` registers the checkout as a
  local `directory` marketplace (never a git remote, so the plugin comes from the same checkout as the binary) and
  installs or updates `canvas@canvas` from it. `admin deploy canvas` installs
  the `canvas` binary and then runs `canvas integrations install` for each
  detected agent, so the binary and the plugin always come from the same commit. Bump `plugin/.claude-plugin/plugin.json`'s
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
