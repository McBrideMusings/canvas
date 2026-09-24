# Viewer

The plain HTML/JS viewer at `/` renders whatever canvasd holds: a stub sidebar, and a
main column stream of turn cards, newest first, updated live over SSE.

## Sub-features

- `empty-state` — no cards yet.
- `populated-state` — cards render with header, HTML posts, images, link rows, path rows.
- `disconnected-state` — banner shown while SSE is down.
- `html-sandbox` — HTML posts render in `<iframe sandbox="allow-scripts" srcdoc=...>`
  with the required CSP, auto-sized via postMessage.
- `image-row` — images load via `/api/file?path=`, click opens a full-size overlay.
- `link-row` / `path-row` — click opens via `/api/open`; path rows also have a Copy
  button.
- `live-update` — a new card arriving over SSE inserts/updates without a page reload.

## How to get to it (user POV)

Open `http://127.0.0.1:8229/` (or the spare-port equivalent while verifying) in a
browser. Everything on the page is driven by canvasd's API; there's no separate viewer
build or deploy step.

## Driving it with Playwright

Preconditions: canvasd running, seeded per `SKILL.md`'s Drive section.

- **Empty state** — navigate to a freshly launched (unseeded) instance,
  `browser_take_screenshot` → copy reads exactly "No posts yet. Canvas shows links,
  files and images from your Claude sessions as they work."
- **Populated state** — seed two sessions, a turn with a link + an existing local path +
  an image path (e.g. `docs/spikes/sideshow-reference/populated.png`), and an explicit
  post containing a `<script src="https://cdn.jsdelivr.net/npm/mermaid@10/...">` diagram
  → screenshot shows: sidebar with "All" + both session names, one card per session,
  sorted newest first, the mermaid diagram rendered inside its iframe, the image inline,
  the link as a blue row, the path as a row with a Copy button.
- **HTML sandbox** — inspect the rendered iframe's `sandbox` attribute
  (`browser_evaluate` or a snapshot) → contains `allow-scripts` and never
  `allow-same-origin`. View the iframe's document source (via `srcdoc`) → contains a
  `Content-Security-Policy` meta restricting `script-src` to cdnjs/jsdelivr/unpkg.
- **Copy button** — click it → button text becomes "Copied", then reverts after ~1.5s.
- **Image overlay** — click an inline image → full-size overlay appears; click it or
  press Escape → overlay closes.
- **Disconnected state** — with a tab open, kill canvasd, wait ~2s,
  `browser_take_screenshot` → banner reads exactly "Reconnecting to canvasd…".
- **Live update** — with a tab open on an empty instance, POST a new turn via curl in
  another terminal → without reloading the page, re-screenshot → the new card is present
  at the top.

## Gotchas

- Headless Chrome's own `--headless --screenshot` flag hangs on this page: `/api/events`
  is a long-lived SSE connection, so Chrome's network-idle wait never resolves even with
  `--virtual-time-budget`. Use the `playwright` MCP tools' `browser_take_screenshot`
  instead — it doesn't wait for network idle.
- Agent-authored HTML must never be set via `innerHTML` on the viewer's own document —
  only ever passed through `iframe.srcdoc`. If a future change renders link/path text
  with anything but `textContent`, that's a regression worth flagging even if it "looks
  fine" on screen.
- `docs/spikes/sideshow-reference/` is deleted by the Canvas MVP land ticket — if it's
  gone, the populated-state comparison has no reference frame to diff against; note that
  in the report rather than treating it as a failure.
