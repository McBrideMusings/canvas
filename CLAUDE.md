# Canvas — agent guide

Canvas is one live stream of posts from every Claude Code session on this Mac:
links, file paths, images, and HTML plans or docs. It is meant to sit open beside
the terminal all day.

## Map

- `canvasd/` — Rust daemon (axum). Listens on `127.0.0.1:8229`, holds the newest
  500 posts in memory, pushes new posts to viewers over SSE, serves `viewer/`.
  Runs as a launchd agent (`com.piercemakes.canvasd`).
- `app/src-tauri/` — Tauri 2 shell: one WKWebView window plus a menu bar icon,
  pointed at `canvasd`. Its own Cargo workspace, outside the root one. It holds
  no state; quitting it loses nothing.
- `viewer/` — plain `index.html` + JS, no build step. Sidebar of sessions
  ("All" by default), stream of turn cards.
- `cli/` — the `canvas` binary. `canvas hook session-start|session-end|stop` reads
  Claude Code hook JSON on stdin: SessionStart and SessionEnd register sessions,
  and Stop turns each finished turn into a card. `canvas post <file|->` posts HTML
  from a file or stdin into the calling session's open card, and unlike a hook
  it fails loudly — one line on stderr, non-zero exit — on any error.
- `scripts/canvasd-service.sh` — installs and controls the launchd agent with
  plain `launchctl`; `admin deploy` and `admin service` call it.
- `skill/canvas/` — the skill agents load to know how to post.

## Rules

- A hook never slows a Claude session: every request to `canvasd` has a 1s
  timeout and the hook exits 0 on any failure.
- HTML posts render in `<iframe sandbox="allow-scripts">` (no `allow-same-origin`)
  with a CSP that allows scripts only from cdnjs, jsdelivr and unpkg and no
  `connect-src`. Never set post markup as `innerHTML` in the viewer's own origin.
- A turn with no links, paths or images, and no explicit post, produces no card.

## Commands

`admin.toml` is the source of truth; run tasks with `admin <cmd>` (`admin check`
lists them). Issues are tracked in beads (`bd ready`).
