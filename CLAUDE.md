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
  pid is gone; sessions with no pid are never swept), SSE push to
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
  The card menu's "Export post…" calls `export_card` (`export.rs`; `capabilities/export.json` grants it to
  the main window alone): it fetches
  `GET /api/cards/:id/export` over the socket (waiting `EXPORT_DOWNLOAD_SECS`
  plus 15s), opens a save dialog from Rust (`tauri-plugin-dialog`; the viewer
  holds no dialog permission), writes the page and returns `{path, warnings}`
  (the toast names what the warnings left out: "Exported to f.html without 1
  image and 2 CDN files"), `null` for a cancelled dialog, or a one-line reason
  the item shows as "Export failed: …";
  it logs `post exported`, `post export cancelled` or `post export failed`.
  A debug build with `CANVAS_DEBUG_DIR` set (`debug.rs`; `verify-app.sh` sets
  it) runs each `eval/*.js` in that folder in the main window, so a script can
  drive the viewer without taking focus (`admin verify-app eval <js|->`), and a
  `save-path` file there answers the save dialog (empty for cancelled).
  `CANVAS_DEBUG_WINDOW=x,y` moves the main window there at start (onto a Retina
  screen). `scripts/clip-demo.sh` is the looping demo for a recorded clip: a
  throwaway daemon and app, four made-up cards, period 5.6s, starting at t0 once
  the file `ADMIN_CLIP_START` names exists when `ADMIN_CLIP=1`.
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
  `theme.css` the palette; with nothing stored the system decides). `theme.css`
  builds on the public tokens in `canvas-tokens.css` (Television's names);
  canvasd serves those plus `canvas-base.css` (base element styles) as
  `/canvas.css`, which an artifact may link (its CSP allows that one URL) and
  `canvas post` inlines into a post from the binary's own copy. `theme.js` also
  calls the app's `set_app_theme` with the stored choice (none: the system), so
  Canvas.app's appearance, and with it `prefers-color-scheme` in every frame,
  an artifact's pane included, follows the theme the window shows. A card
  always renders in the active theme: `buildIframeDoc` rewrites its
  `prefers-color-scheme` queries, forces the page surface and ink, and recolors
  text that falls below 4.5:1 contrast; a change rebuilds every card iframe.
  A card image opens in a lightbox (`openLightbox` in `app.js`): a click on the
  image toggles fit and 2.5x, wheel or pinch zooms at the cursor (1x to 8x),
  drag pans while zoomed, `+` `-` `0` zoom and reset, and only the X button,
  Esc or a click on the bare backdrop closes it.
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
  stdout: `{"card_id": "...", "images": [...], "targets": [...]}`, plus
  `"viewers": N` with `--focus`. A local
  `<video src>` or `<source src>` joins `images` and is served by the same
  `/api/cards/:id/images/:n` route (which honours `Range`, since a video needs
  it); the viewer's lightbox skips those entries and the card CSP carries
  `media-src`. A local `<link rel="stylesheet" href="/abs/x.css">` is replaced
  by a `<style>` block with its `url()` assets as data URIs, before the scan
  (`cli/src/inline_css.rs`); one inside SVG or MathML, which the browser never
  loads, stays as written. Hook
  dispatch never starts a tokio runtime; only `canvas daemon` does.
  `canvas card <card_id>` prints one post as JSON (`GET /api/cards/:id`), so
  a `canvas-post://<card_id>` link from a post's "Copy post link" menu item
  can be read by an agent.
  `canvas export <card_id> [-o file]` (`GET /api/cards/:id/export`,
  `canvasd/src/export.rs`) writes one post as a standalone page, to
  `<card_id>.html` by default, and prints `{"path", "cards": 1, "warnings"}`.
  Each `/api/cards/:id/images/:n` src becomes a base64 `data:` URI read from
  the card's stored path (a missing file is a `missing-image` warning and an
  "image missing" placeholder; a file that would take the page's media past
  `MAX_MEDIA_BYTES`, 32 MB in all, is refused from its size before any byte
  is read (`export::read_media`) and is a `media-too-large` warning naming
  the file's size and the cap ("a 40.0 MB file is over the 32 MB…", or
  "would take the page past" when it fits alone) with an "image left out"
  placeholder, while later files that fit still go in; a video or source
  left out either way loses its src), each `#canvas-open-<n>` anchor becomes a real
  `target="_blank"` link (http/https) or plain text (a local path), and a shim
  delivers the card's latest `canvas data` value as `canvas-data` and swallows
  `canvas-reply`. Each `<script src>`, `<link rel="stylesheet">`, `@import`
  and `url()` on the card CSP's hosts (`canvasd/src/cdn.rs`: cdnjs, jsdelivr,
  unpkg, Google Fonts) is downloaded over HTTPS (10s limit, `MAX_ASSET_BYTES`
  cap; each request looks its host up once within that limit, before any connection's
  setup time starts, waiting on that host's lookup when one is already running, so a
  hung resolver holds one thread per host; a connection whose TCP connect or TLS handshake outlasts half the time
  left is dropped for a fresh one, at most three, logging `export fetch setup
  stalled`; a module's specifiers and a stylesheet's `@import`s and `url()`s resolve
  from the URL its download landed on after redirects, `cdn::Fetched`) and inlined —
  classic scripts and styles as blocks, fonts as `data:` URIs,
  a `<script type="module" src>` and every module it reaches on those hosts as
  `data:` URIs in one `<script type="importmap">` in the head (each module's
  specifiers read by oxc, `canvasd/src/esm.rs`, each one resolved from the module's
  URL through the card's own import maps, scopes included (`canvasd/src/import_map.rs`,
  the standard's resolution) and, once every card map is read, written as the
  absolute URL it leads to, or, where the page's map would send that URL
  elsewhere, as the reserved name `canvas-export:<url>` the page's map holds
  exactly (`PageMap::specifier`); one that leads to nothing the card could load (an
  address relative to the page, which the card CSP refuses, a blocked entry, a bare
  name with no entry) is written `canvas-export:blocked`, mapped to null; the
  element becomes `import "<url or name>";`; the card's own import maps merge into that one, theirs winning,
  each of their addresses naming a fetched module rewritten to its `data:` URI;
  every module those maps name, and every one the card's inline `<script
  type="module">` imports, is fetched too, a name the inline module used getting
  its own entry; a module that can't be fetched
  or parsed staying on the network with a `fetch-failed` warning; a fetched
  module that computes an import from its own address — `import()` of anything
  but a plain string, `import.meta.url`, `import.meta.resolve`, or `import.meta`
  used any way but reading another named property — is still
  inlined, with one `computed-import` warning naming it and the first place,
  since that import fails from a `data:` URI; the viewer's toast adds "1
  module's computed imports will fail"), an
  `@import`'s `layer`, `supports()` and media conditions as `@layer`,
  `@supports` and `@media` blocks; the CSS scan skips strings, comments and
  escapes as the browser does. An imported sheet is first closed as its own end
  of file closes it (`closed_at_end`: open strings, comments, `url(`, brackets
  and blocks, an at-rule's `;`; a selector with no block dropped), so it can't
  run into the text after it. A `<style>` inside SVG or MathML holds markup,
  not raw text: as Canvas.app's WebKit reads it, its CSS is every text run and
  CDATA section inside it joined, child elements' included, up to the tag that
  closes it (`</style>`, an ancestor's end tag, a breakout tag such as `<p>`;
  `Tags::is_open` says), comments
  (bogus ones too) dropped, every HTML character reference (named or numeric,
  `htmlize` via `canvas_core::html::decode_entities`) decoded once per text run
  outside CDATA, and the scan reads it whole (`Cdn::markup_css`), so a reference
  split by a comment, a child element or a CDATA boundary is still one. The pieces before the
  first replacement and after the last stay as written; those between become one
  run in the form of the first of them, text written with entities, CDATA with
  `]]>` split or a child's raw text, followed by the child elements' tags, emptied; a style where nothing was inlined keeps its text exactly as
  written. Inside such a style's `<foreignObject>` an HTML script runs and a
  stylesheet link applies, but their text is the style's CSS, so only attributes
  change there: a CDN `<script src>` or stylesheet `href` becomes a `data:` URI
  (a module's an `import` of its import-map key), a card file a `data:` URI or,
  when missing, no `src` at all, anchors as anywhere else. A failure, or an `@import` nested past
  `MAX_IMPORT_DEPTH`, keeps the link and adds a `fetch-failed` warning; so does an
  `@import` with conditions whose sheet still holds an `@import` that stays a link,
  since the browser ignores an `@import` inside a block, and every `@import` inlined
  as rules ahead of a later one in its sheet that writes a link, since the browser
  ignores an `@import` after rules; for the same reason an `@import` after its
  sheet's own rules stays as written, never fetched. `export_card` drops a
  `fetch-failed` warning whose URL's content went into the page through another
  reference (`Cdn`'s list of inlined URLs). Each download
  logs `export fetch` (with `from`, the URL it landed on) or `export fetch failed`. A debug build fetches through
  `CANVAS_CDN_ORIGIN` when it is set (tests' stand-in CDN); a release build
  ignores it. None of `buildIframeDoc`'s theming, sizing or sandbox is
  carried; `prefers-color-scheme` rules stay, so the reader's system picks.
  The route answers `{html, warnings}` (`ExportResult` in `canvas-core`,
  which also holds the shared `base64` and the `html` tag scanner, with
  `card_title`: a card's first heading, else first line of text, else "Canvas post"); canvasd
  logs `card exported`.
  `canvas export --all [--session <id>] [-o file.zip]` (`cli/src/export.rs`)
  reads every card canvasd holds from `/api/state` (or one session's; an
  unknown id exits 1), exports each through that route one at a time, and
  writes a zip (`canvas-export.zip` by default; stored entries, the `zip`
  crate): `<card_id>.html` per card plus `index.html`, newest first by `at`,
  each row linking its page with the session name, `card_title` and the time,
  self-contained and following the reader's light/dark setting. It prints
  `{"path", "cards", "warnings"}`, each warning carrying its `card_id`; a card
  evicted mid-run is a `card-gone` warning and one whose export canvasd answers
  with an error an `export-failed` warning, neither getting a page. The zip is
  written to `<file>.part` and renamed, so a failed run leaves the target as it
  was. The CLI logs `export all`.
  `canvas focus <card_id>` (`POST /api/cards/:id/focus`, 404 for an unknown
  id) publishes a `card-focus` SSE event, never persisted; each viewer runs
  the same `openCardLink` a `canvas-post://` link does — clear search and
  filters, scroll, ring — without showing or raising the window. It prints
  `{"viewers": N}`, the open `/api/events` streams the event reached, and
  exits 1 with one stderr line when N is 0. `canvas post --focus` (also with
  `--update`) focuses the card it just wrote and adds `viewers` to its JSON;
  zero viewers there is a stderr warning, not a failure, since the card exists.
  `canvas snapshot <card_id> <out.png>` (`POST /api/cards/:id/snapshot`)
  writes the PNG of the card as the open window renders it, in its theme, and
  prints `{"path", "width", "height", "clipped"}` (pixels). canvasd publishes a
  viewer-only `card-snapshot` event `{id, request}` and holds the request up to
  20s; the viewer takes the requests one at a time, waits up to 3s for the card
  to settle (iframe sized, arrival slide and ring over), scrolls it into
  `#stream`'s view (no ring, nothing cleared) and calls the app's
  `snapshot_reply` command with the card's rect clipped to that view and
  whether it was clipped, or with why it can't show it (its session
  hidden, filtered by the search). `app/src-tauri/src/snapshot.rs` captures the
  rect with WKWebView's `takeSnapshot` and posts the PNG (`x-canvas-clipped:
  true` for a card taller than the window), or the reason as `{"error"}`, to
  `POST /api/snapshots/:request`, which answers the first reply and 404s any
  later one. No viewer is a 409, a reason a 422, no answer a 504; the CLI
  prints canvasd's one line and exits 1. A clipped capture exits 0 with a
  stderr note.
  `canvas theme light|dark` (`POST /api/theme`, any other value refused)
  publishes a viewer-only `theme-set` event; each viewer calls
  `window.canvasTheme.set`, the same path as a click on the theme button, so
  the choice persists in the viewer's localStorage like a click's (the
  Settings window follows through the `storage` event). It prints
  `{"viewers": N}` and exits 1 with one stderr line when N is 0. The viewer
  reports the theme it shows with `PUT /api/theme` after every state load and
  every change; canvasd holds it in memory only, and `GET /api/theme` (bare
  `canvas theme`) prints `{"theme": ...}`, exiting 1 when no viewer is open
  or none has reported. The unbundled dev app keeps its localStorage
  under `~/Library/WebKit/app`, apart from the installed app's
  `~/Library/WebKit/com.piercemakes.canvas`.
  `canvas post --update <card_id> <file>` sends a PUT to `/api/cards/:id`
  instead, replacing that card's html/images/targets in place (same id,
  session_id and `at`) rather than creating a new one; 404s if the id doesn't exist.
  It sets the card's `updatedAt` (absent until then), and the daemon logs
  `card updated`. The Timeline orders cards by `updatedAt`, else `at`
  (`Card::touched_at`, which age pruning reads too), so an updated card moves
  to the top; its header reads "posted 40m ago · updated 3m ago", clock times
  in the tooltip.
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
  The window's always-on-top toggle is "keep on top"
  (`get_keep_on_top`/`set_keep_on_top`).
- Artifacts (ADR-0002, `canvasd/src/artifacts.rs`, `artifact_routes.rs`): a web
  page an agent keeps until someone deletes it. An owned one is a folder canvasd
  owns at `artifacts/<id>/` in `CANVAS_DATA_DIR`, its id `art-` plus 10 hex
  digits; the records (id, title, kind `owned` or `linked` with its `link`,
  created/updated times) persist in `artifacts.json` beside `profiles.json`,
  loaded by `AppState::open`, never evicted or pruned by age. `canvas artifact
  new [--title t]` prints the record with its folder `path`; `new --link <path>`
  instead records an existing absolute folder or `.html` file the person owns
  (refused when it doesn't exist): canvasd serves and watches it but never
  writes to it, so `put` refuses a linked artifact and `delete` leaves its
  files, and a linked HTML file is the whole artifact (only `/artifacts/<id>/`
  reaches it). A linked path that no longer exists is a state, never an error:
  the view carries `"sourceMissing": true`, the list row a warning mark, and the
  pane "Source missing" with the path and the relink command; `relink <id>
  <path>` repoints a linked artifact, keeping its id, and watches the new path.
  A link that moves away and back while canvasd runs recovers by itself; a path
  the OS refused to watch (missing when canvasd starts) is retried every second
  and, once watched, reloads as a save would. `put <id>
  <file|dir>` sends canvasd the absolute path
  and canvasd copies the file, or the folder's contents, in (a symlinked folder is
  skipped, a symlinked destination replaced) and stamps `updatedAt`. Saving a
  file into the folder does the same with no command: `canvasd/src/watcher.rs`
  watches each artifact's path (owned folder or link) recursively (notify,
  FSEvents), and a write belongs to every artifact whose path holds it, so
  nested links all reload; it ends a burst
  after 200ms without a write (2s at most), and stamps `updatedAt` and
  publishes `artifact-upserted` once, unless the folder's fingerprint (paths,
  sizes, mtimes, inodes, in memory) still matches its last stamp; a `put` holds
  its artifact's bursts (`Watcher::hold`) until it has stamped, so its own
  writes never reload twice; `list`,
  `show <id>` and `delete <id>` (record and folder) round it out, each failing
  loudly with canvasd's error text. Every create, put, relink, watched change
  and delete appends a line (`at`, `action`, `sessionId`, `agent`, `pid`) to
  `artifact-log.jsonl` in `CANVAS_DATA_DIR` (`canvasd/src/provenance.rs`); the
  CLI sends its agent's session, agent and pid as `x-canvas-session`,
  `x-canvas-agent` and `x-canvas-pid` headers, and a watched change or a
  request without them logs nulls. The lines outlive the artifact; `canvas
  artifact log <id>` (`GET /api/artifacts/:id/log`) prints them as JSON, 404
  for an id with no record and no lines. The viewer never shows them. `show` adds `entry` (`index.html`, else the
  folder's only top-level HTML file) and `size`, read on every request from the
  entry's `<meta name="canvas-size" content="WxH">`. canvasd serves the files at
  `/artifacts/<id>/<path>` (`/artifacts/<id>/` is the entry) after refusing a
  `.`/`..` segment and any path whose resolved target leaves the folder, with a
  CSP header (scripts, styles and fonts from the folder, inline or the three
  CDNs; `connect-src 'none'`, `form-action 'none'`, `sandbox allow-scripts`) and
  `Access-Control-Allow-Origin: *` for module scripts in the pane's opaque origin;
  `bridge.rs` passes those two headers through. `artifact-upserted` and
  `artifact-removed` SSE events are viewer-only; `/api/state` carries
  `artifacts`. `canvas focus <art-id>` (`POST /api/artifacts/:id/focus`,
  `artifact-focus`) switches every viewer to the Artifacts page on that artifact
  and rings its frame; `--focus` on `artifact new|put` does the same once the
  artifact is written and adds `viewers` to the JSON (zero is a stderr warning,
  as for `post --focus`). `canvas snapshot <art-id> <out.png>` (`POST
  /api/artifacts/:id/snapshot`, the viewer-only `artifact-snapshot` event)
  shares the card snapshot's wait and `/api/snapshots/:request` reply: the
  viewer waits up to 3s for the page to load and the ring to end, then hands
  the app the open frame's rect at whatever size it shows (chosen, declared,
  full window), or refuses when the Timeline or another artifact is showing,
  naming `canvas focus`.
  Script errors: canvasd serves every artifact HTML page with
  `canvasd/src/error_relay.js` first inside `<head>` (joined onto one line so
  the page's line numbers hold) and `crossorigin` added to each `<script src>`
  lacking one. The pane's opaque origin makes WebKit mask nearly every error as
  "Script error.", so the relay wraps timers, animation frames, microtasks,
  listeners, `on*` properties and observers, reports the real error from its
  stack, rethrows, and drops the masked copy; it posts `canvas-artifact-error`
  `{kind, message, source, line, column}` to the viewer, which relays it
  to `POST /api/artifacts/:id/errors` only when the sender is the pane's
  frame, under the artifact that frame was built for (switching artifact
  builds a new frame, so an unloading page can't report onto the next). canvasd keeps the newest 50 per artifact
  in memory (dropped on delete), logs `artifact script error`, and `show`
  lists them as `scriptErrors`.
  Links: the same relay catches a click on an `<a>` whose resolved `href` is
  `http(s)` (relative links resolve to `canvas:` and navigate inside the pane),
  prevents the navigation and posts `canvas-artifact-open` `{url}`; the viewer
  relays it, for the pane's frame only, to `POST /api/artifacts/:id/open`.
  canvasd refuses anything but `http(s)` of at most 2KB (400) and a second
  open from one artifact within a second (429, since a page script can post the
  message with no click), runs macOS
  `open` on it (`routes::open_target`; a debug build runs `CANVAS_OPEN_BIN`
  instead when set), logs `artifact link opened`, and keeps the newest 50 in
  memory (dropped on delete); `show` lists them as `openedLinks`.
  Widgets and refresh: `canvas artifact new|put --widget <file> --refresh '<cmd>'
  [--every <secs>]` (5s minimum, default 30s; a flag left off keeps what the
  record has) stores `widgetHtml` and `refresh: {command, everySecs, cwd, pid}`
  on the record in `artifacts.json` — `cwd` is the CLI's working directory,
  `pid` the `x-canvas-pid` header. `canvasd/src/refresh.rs` (a 1s tick) runs
  `sh -c <cmd>` in `cwd` while `pid` is alive (always, with no pid), one run at
  a time, killed after 60s: stdout JSON is kept as the artifact's `data` and
  published as `artifact-data`, as `canvas data <art-id>` (`PUT
  /api/artifacts/:id/data`, up to 256KB, answers `{"viewers": N}`) does;
  anything else sets `refreshError` `{message, at, retryAt}` (first stderr line
  or the reason; cleared by the next success, published as `artifact-upserted`,
  logged `artifact refresh failed`) and doubles the wait per failure up to 10
  min. The refresh persists with the record, so it runs again after a daemon
  restart (checked against the same pid); an agent's exit stops it and clears
  its `refreshError`. `data` and `refreshError` live in memory and ride on the artifact's view
  (`/api/state`, `show`). The viewer posts `data` into the widget and the
  open pane as `{type:'canvas-data', value}`, and again after either frame loads.
  The viewer's title bar has a `Timeline | Artifacts` switch (`showPage`, the
  choice in localStorage); the toolbar shows only on the Timeline. The Artifacts
  page lists every artifact, most recently changed first, beside the open one's
  pane. A row (title, muted time, then the widget, under one transparent button)
  is kept across renders and patched in place (`artifactRows`): its widget is
  `widgetHtml` in a sandboxed iframe built like a card's, as tall as its content
  up to 120px, rebuilt only when the HTML or theme changes, and reloaded only
  when the row moves. A refresh error dims the widget, adds a red dot and a
  "refresh failed" line, and puts "Refresh failed 2m ago: … · retrying in 4m"
  under the pane header. The Timeline shows no widgets. The pane is an `<iframe sandbox="allow-scripts">` on its URL, white behind the page,
  at its `canvas-size` clamped to the pane or else filling it. A watched change
  or `put` stamps `updatedAt` and its `artifact-upserted` event carries
  `changed`, the paths written relative to the artifact's path (no folders,
  hidden names, `~` backups or extensionless names gone by the burst's end;
  absent past `MAX_CHANGED_PATHS`). While the open page's relay listens (from
  its `canvas-artifact-location` with `start: true` until its
  `canvas-artifact-unload` on pagehide; it reports the address again on every
  navigation), the viewer never reloads the frame for a change: it posts
  `canvas-artifact-changed {stamp, paths}` (paths null when unknown, such as
  changes made while it was disconnected), the relay answers
  `canvas-artifact-ack {stamp}` and dispatches a cancelable
  `canvas-artifact-changed` event on the page's window; uncancelled, a change
  only to linked stylesheets swaps them in place (a sheet that fails to load
  reloads) and anything else runs `location.reload()`, keeping path and hash.
  A relay that starts while the newest stamp is unacknowledged hears that
  change again. With no relay listening the viewer sets the frame's src to
  the page's last reported address (in memory, per artifact), where a rebuilt
  frame also opens; a load the viewer caused that brings no start hello (the
  address is gone) drops it and opens the entry page. Its menu copies the id or folder path and deletes it. A grip at the
  frame's bottom-right corner drags it to another size (clamped to the pane,
  160×120 at least; a double-click goes back to the declared size), kept per
  artifact in the viewer's localStorage (`canvas.artifact-sizes`); the header's
  full-window button hides the list and header so the frame fills the window
  below the title bar, which then carries the title and an "Exit full window"
  button; Escape or that button returns. `canvas artifact pane <id>
  --size WxH|--reset|--full|--exit` (`POST /api/artifacts/:id/pane`, the
  viewer-only `artifact-pane` event) does the same in every viewer; the viewer
  reports the frame's size, `full` and `chosen` with `PUT /api/artifact-pane`
  after every change and state load, held in memory, and `DELETE`s it when no
  pane is showing; bare `canvas artifact pane` (`GET`) prints it, exiting 1
  when no open viewer reports one. A chosen size is kept as asked and clamped
  to the window only on screen. Escape reaches the viewer only while focus is
  outside the page's iframe; entering full window focuses the exit button.
  The menu's "Reset artifact" and `canvas artifact reset <id>` (`POST
  /api/artifacts/:id/reset`, 404 for an unknown id) take one path: canvasd
  drops the artifact's held `data` and `scriptErrors`, logs `artifact reset`
  and publishes the viewer-only `artifact-reset` event, and every viewer
  rebuilds that pane's frame at the entry page, forgetting its remembered
  address and unacknowledged change stamp. Files, the chosen size and full
  window stay; a refresh's next run brings `data` back. It prints `{"viewers":
  N}`; zero viewers is a stderr note, not a failure, since the held state is
  gone either way.
- Logs: `canvas-core/src/log.rs` writes `<UTC time> <process> <LEVEL> <message>
  key=value ...` lines to `daemon.log`, `cli.log` or `app.log` in `logs/` under
  `CANVAS_DATA_DIR` (`canvas logs --path` prints it), rotating a file at 5 MB and
  keeping three old copies. A process writes nothing until it calls `log::init`, so
  canvasd's in-process tests never touch the real data dir. canvasd logs every
  request (an axum middleware in `canvasd/src/lib.rs`: method, path, status, ms, and
  the body of a 4xx/5xx as `error`) plus start, reload, store, sweep and refresh
  failures; `canvas_core::unix_http::call` logs each request it sends (status, or
  `canvasd unreachable` with the reason): every CLI call to canvasd and the app's
  export fetch; the CLI also logs each hook's outcome; the app logs bridge
  failures, event-stream changes, daemon install and integration sync. Every write
  failure is swallowed. New features log through it.
- `canvas integrations list [--json]|install <agent> [repo]` (`cli/src/integrations/`)
  detects each agent (its CLI on PATH), reports `current`, `out of date`,
  `not installed` or `needs review`, and installs; per agent it is
  `AgentAdapter::{detect, status, install}`. Codex's install merges Canvas's
  SessionStart, UserPromptSubmit and SessionEnd groups into
  `~/.codex/hooks.json` (`$CODEX_HOME`), replacing only its own groups and
  appending new ones so other tools' entries keep their positions, and writes
  `canvas-hooks-version` beside it. Codex's sandbox (`read-only` and
  `workspace-write`) refuses the socket connect, so install also writes
  `rules/canvas.rules` there, a file Canvas owns whole: a `prefix_rule` that
  runs the subcommands which only talk to canvasd (`post`, `data`, `focus`,
  `wait`, `replies`, `card`, `theme`, `artifact`, `profile`, `guidance`; not
  `export` or `snapshot`, which write anywhere) outside the sandbox. Codex
  matches it only for a plain `canvas …` command with no pipe or heredoc, which
  the guidance tells agents to use; a missing or changed file reads as
  `out of date`. Codex runs a hook only after the user
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
  installs or updates `canvas@canvas` from it. `admin deploy` (`scripts/deploy.sh`) installs
  the `canvas` binary and then runs `canvas integrations install` for each
  detected agent, so the binary and the plugin always come from the same commit;
  it then quits a running Canvas.app, replaces `/Applications/Canvas.app` and
  opens it again in the background (`open -g`). Bump `plugin/.claude-plugin/plugin.json`'s
  `version` when `plugin/` changes, or `claude plugin update` keeps the old copy.

## Rules

- A hook never slows a Claude session: every request to the daemon has a 1s
  timeout and the hook exits 0 on any failure. A log line that can't be written
  changes nothing a command prints or how it exits.
- HTML posts render in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`)
  with a CSP that allows scripts only from cdnjs, jsdelivr and unpkg and no
  `connect-src` — a card can still hand one value back via `canvas-reply`
  (canvas-17z), but only by posting it up to the viewer, never by fetching
  anything itself. Never set post markup as `innerHTML` in the viewer's own origin.
- Every `canvas post` creates its own card; nothing creates a card automatically.
- canvasd refuses a post or update (400, logged `card refused`) whose HTML would
  freeze Canvas.app's WebKit (`canvas_core::html::webkit_freeze`: an end tag
  closing a table, section or row while the cell mode's cell is one the
  512-element depth cap already closed, or a table part's start tag, or its
  end tag in table scope, while "in select in table" holds a select the cap
  already closed, or a page that makes WebKit reopen more closed formatting
  elements before text or a start tag than 100,000 plus one per 3 bytes read,
  a tree larger than the page's own markup could build), and drops such a card from
  `stream.jsonl` on reload (`card dropped on reload: it freezes WebKit`). A
  widget is framed like a card, so `canvas artifact new|put --widget` refuses
  one the same way (`widget refused: …`) and `artifacts.json` loads it as no
  widget (`widget dropped on load`). An artifact's HTML page is a whole
  document with no wrapper `<div>`, so its cap holds one element more
  (`webkit_freeze_page_reason`, which reads the file's bytes as WebKit decodes
  them, each invalid UTF-8 sequence one U+FFFD, and names the file's own byte
  offset); canvasd answers such a page with a 422 whose
  plain text, shown in the pane, says why (`artifact page refused`), and leaves
  the file as it is. The reason
  (`webkit_freeze_reason`) names the tag and its byte offset, never the
  tag's attributes, or "the end of the page" when the text after the last tag
  is what reopens them.
- An artifact's page is never themed, width-capped or sized to its content by
  the viewer, and runs in `<iframe sandbox="allow-scripts">` without
  `allow-same-origin`. canvasd serves its files only from inside its folder
  (no `..`, no symlink out) and always with the artifact CSP; never serve them
  without it. The only change canvasd makes to a served page is to an HTML
  file: the error relay first inside `<head>` and `crossorigin` on each
  `<script src>` lacking one, or the refusal in place of a page that would
  freeze WebKit.
- canvasd has no TCP listener, so nothing reaches it except a process that can
  open its 0600 socket, and its routes carry no Host or Origin checks. Never add
  a TCP or other network listener to it; a client that needs it goes through the
  socket (Canvas.app's `canvas://` proxy is the only bridge for a webview).

## Commands

`admin.toml` is the source of truth; run tasks with `admin <cmd>` (`admin check`
lists them). Issues are tracked in beads (`bd ready`).
