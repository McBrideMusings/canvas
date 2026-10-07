---
name: canvas
description: Post a self-contained HTML card to Canvas, the live stream of Claude Code sessions on this Mac, when the answer needs a figure chat can't carry — a diagram of a structure or flow, a chart or table of numbers, an image or before/after, an annotated diff, a control the user answers from. Build it as a document (panels, headings that tell the story, inline SVG), following DOCUMENTS.md. Don't post for volume alone: a changed file, long code, a list of links or a closing report as Markdown blocks stays in chat.
---

# Canvas

Canvas is a stream of cards, one per `canvas post` call, shown in a viewer
that sits open beside the terminal. Cards from every session share one
stream, newest first; a chip per session filters it. Nothing creates a card automatically — a card exists only because a
post created it.

A card is a document the user reads, not a second copy of the chat. Post one
only when it carries a figure chat can't, and build it the way
[DOCUMENTS.md](DOCUMENTS.md) describes: numbered panels, headings that tell
the story when read alone, the structure drawn as inline SVG.

## How to post

Run `canvas post` with the content as a file argument, or piped in on
stdin:

```
canvas post /path/to/report.html
canvas post /path/to/notes.md
echo 'done' | canvas post
canvas post - <<'EOF'
## Task: migrate the schema
- [ ] step one
EOF
```

In a sandboxed Codex session, write the content to a file first and pass its
path, calling the binary as plain `canvas`: Codex's sandbox blocks canvasd's
socket, and the rule Canvas installs to allow it matches only a plain
`canvas <subcommand> …` command, never a pipe, a heredoc or
`~/.local/bin/canvas`.

Markdown and text exist for a status checklist you keep current with
`--update`; a card that explains something is HTML.

`--update <card_id>` replaces an existing card's content in place instead of
creating a new one — the `card_id` a prior `canvas post` reported in its
JSON output. Use it for a card that represents one ongoing unit of work (a
long task's status, a running checklist) rather than posting a fresh card
every time it changes — the card keeps its id and posted time, gains an
`updatedAt`, and moves to the top of the Timeline in every open window, its
header showing both times, without disturbing any other card. There
is no separate threading concept: a card you keep updating with `--update`
*is* the thread.

```
canvas post - <<'EOF' # first post
## Task: migrate the schema
- [ ] step one
EOF
# -> {"card_id": "c1", ...}

canvas post --update c1 - <<'EOF' # later, same card
## Task: migrate the schema
- [x] step one
- [ ] step two
EOF
```

`canvas focus <card_id>` brings a card into view in every open Canvas window:
it clears the search and filters hiding it, scrolls to it and rings it, but
never raises the window. It prints `{"viewers": N}` and exits 1 when no window
is open. `canvas post --focus` (also with `--update`) does the same for the
card it just wrote and adds `"viewers"` to its JSON; with no window open it
warns on stderr and still exits 0, since the card was written.

`canvas snapshot <card_id> <out.png>` writes a PNG of the card as the open
Canvas window renders it, in the active theme, so you can look at what you
posted. It prints `{"path": "...", "width": N, "height": N, "clipped": false}`
(pixels, at the screen's density). Right after a post it waits a few seconds
for the card to finish arriving. It exits 1 with one stderr line when no window is open, or
when the window can't show the card: its session archived or hidden,
or filtered out by the search (`canvas focus <card_id>` clears both).
Only the part inside the window is captured: a card taller than the window
is cut at the window's bottom edge, and the output says `"clipped": true`.

`canvas theme light|dark` switches every open Canvas window's theme, so a
snapshot can show a card in both. It is the user's own setting and persists
like a click on the theme button: read it first with bare `canvas theme`
(`{"theme": "light"}`) and set it back when you are done. It prints
`{"viewers": N}` and exits 1 when no window is open.

Input is Markdown, plain text, or HTML — pick with `--format md|text|html`,
or let it infer from the file's extension (`.md`/`.markdown`, `.txt`,
`.html`/`.htm`); stdin or an unrecognised extension defaults to Markdown.
Markdown renders with tables and strikethrough, and raw HTML embedded in the
Markdown source passes through unchanged. Text renders as an HTML-escaped
`<pre>` block. HTML passes through as-is and must be self-contained: inline
styles or a `<style>` block. A local stylesheet is the one exception: `canvas
post` inlines it (see "Local stylesheets" below).

Canvas has a light and a dark theme, chosen by a button in its title bar, and
a card always renders in the active one. The viewer forces the page's
background and text color, so don't set `html` or `body` background or color,
and set a `color` beside any background you paint (a callout, a table header).
To adapt on purpose, define a light palette on `:root` and override it under
`@media (prefers-color-scheme: dark)`; Canvas makes that query match the
active theme, not the system setting. Text that ends up unreadable against its
background is recolored at render time.

### The Canvas stylesheet

`<link rel="stylesheet" href="/canvas.css">` gives a post or an artifact
Canvas's own tokens and base element styles (type, headings, links, code,
tables, form controls), light and dark, following the active theme. In a post
`canvas post` inlines it, like any local stylesheet; an artifact loads it from
canvasd and restyles live when the theme changes, with no reload. An artifact
that doesn't link it gets no styling from Canvas. Write your own CSS with these
tokens, each with a literal fallback (`color: var(--color-text, #1a1a1a)`).
The list is closed; don't invent other names:

- Colour: `--color-surface`, `--color-surface-muted`, `--color-text`,
  `--color-text-muted`, `--color-text-reversed`, `--color-border`,
  `--color-primary`, `--color-primary-text` (text on primary), `--color-link`,
  `--tint-primary`, `--color-danger`, `--tint-danger`, `--color-success`,
  `--color-alert`.
- Type: `--font-sans`, `--font-mono`, `--font-weight-normal`,
  `--font-weight-medium`, `--font-weight-semibold`, `--text-sm` (12px),
  `--text-base` (14px), `--text-md`, `--text-lg`, `--text-xl`, `--text-2xl` (24px).
- Spacing and radii: `--space-2`, `--space-4`, `--space-6`, `--space-8`,
  `--space-12`, `--space-16`, `--space-24`, `--space-32`, `--space-48`,
  `--control-radius`, `--panel-radius`, `--radius-pill`.

`button.primary` gets the primary fill. The names match Television's public
tokens, so a page written for one reads in the other.

Code blocks (`<pre>`) render dark with light text. If you paint a background
inside one — diff rows, highlights — set a dark `color` on that element too, so
the text stays readable on it (the viewer corrects low contrast, but say what
you mean).

### Local paths and links

An image at an absolute path that exists — `![](/abs/shot.png)` in Markdown
or `<img src="/abs/shot.png">` in HTML — renders inline in the card, where
you put it. Clicking it opens it full size in the viewer's lightbox, and the
arrow keys step through the card's other images. A link to an existing local
file or to any `http://`/`https://` URL — `[plan](/abs/plan.md)` or
`<a href="…">` — opens on click. A link to a local file whose only content
is an existing local image loses the link, so that image opens the lightbox;
an image inside a link to a URL, or beside other text, opens the link. A local video — `<video src="/abs/clip.webm">`, or `<source src="/abs/clip.mp4">`
inside a `<video>`, in HTML or in Markdown's raw-HTML passthrough — plays inline
the same way (WebM/VP9 and MP4/H.264 play; `.m4v` and `.mov` are served too).
Canvas adds no attributes to your tag; it does not open a video in the lightbox,
since the player has its own controls and fullscreen. Only these forms are
picked up: a bare path in the text stays plain text. An absolute local path that
doesn't exist is left unchanged (a broken image, video or dead link) and `canvas
post` warns about it on stderr — the post still lands.

Animated GIF, animated WebP and APNG files keep animating, inline and in the
lightbox: the daemon serves the file's own bytes.

### Local stylesheets

`<link rel="stylesheet" href="/abs/tokens.css">` is replaced by a `<style>`
block holding that file, and the `url(...)` references inside it — fonts, images,
absolute or relative to the stylesheet — become `data:` URIs, so the card carries
the project's real CSS. Each embedded asset is capped at 512 KB; a larger one, a
missing file, or an asset of an unknown type is left as written and `canvas post`
warns on stderr. `http(s)` links, `data:` URLs and `@import` are not followed. A
relative `href` is not picked up; use the absolute path.

### Motion

Pick the form by what the reviewer needs:

- **Animated image** (GIF, animated WebP, APNG): a loop to glance at. Lowest cost.
- **Video** (`<video src="/abs/clip.webm">`): a recording of one run, with a scrubber.
  Write `controls muted loop playsinline autoplay` for a short silent clip; a
  browser only autoplays a muted video.
- **Live animation card**: the real HTML/CSS/JS running in the card. Use it when
  the reviewer must replay it on demand or judge exact timing and easing, which a
  recording's frame rate blurs.

A live animation card:

- Runs ordinary inline `<script>`; the card's CSP allows inline scripts, and
  external ones only from cdnjs, jsdelivr and unpkg.
- Carries everything itself, because it cannot fetch: CSS inline or via a local
  `<link rel="stylesheet" href="/abs/…">` (inlined for you), fonts and images as
  `data:` URIs, or as a local `<img src="/abs/…">`. A `@font-face` file in a local
  stylesheet is embedded for you; do not rely on any other host.
- Plays on its own in a loop (`animation-iteration-count: infinite`, with
  `alternate` or a pause between runs) and has a visible **Replay** button that
  restarts it: remove the animation class, read `el.offsetWidth` to force a
  reflow, add the class back. Scripted timelines restart from zero the same way.
- Honours `@media (prefers-reduced-motion: reduce)` by showing the end state
  without movement, or by starting paused behind a Play button. Canvas passes the
  system setting through unchanged; it does not rewrite this query.
- Follows the theme rules above: a light palette on `:root`, overridden under
  `@media (prefers-color-scheme: dark)`, no `html`/`body` background or color. The
  card is rebuilt when the theme changes, so the animation restarts then.
- Stays within its frame: an overflowing element is clipped, and `transform`
  moves do not change the card's height.

### Size and layout

The viewer sets a card's width — 846px of content at the default window
size, which is also the most it gets; only a narrower window gives less. The
height is yours. Aim for about one
screen, roughly 600px tall, so the user can take the card in without
scrolling past it.

That target is a default, not a limit. Organise the post however conveys the
information best, and make it taller or shorter when the content calls for
it — a long diff review can run long, a one-number result can be a few lines.
Decide the size on purpose rather than letting it fall out of the content.

Because every image opens full size in the lightbox, an image doesn't need
to fill the card to be readable:

- Give a screenshot a display width — `<img src="/abs/shot.png" width="360">`
  (raw HTML passes through Markdown too) — so a tall phone capture doesn't
  push the card to several screens.
- Put a before/after pair or a set of related shots side by side, e.g. in a
  `<div style="display:flex;gap:8px">`, rather than stacking them.
- Leave an image full width only when its detail is the point of the post.

### Sandbox limits

Posted HTML renders in `<iframe sandbox="allow-scripts">` (no
`allow-same-origin`) with a CSP that allows scripts only from
`cdnjs.cloudflare.com`, `cdn.jsdelivr.net`, and `unpkg.com` — pin a library
to an exact version from one of those. There is no `connect-src`, so the
page cannot `fetch()` or open a `WebSocket`; everything it shows has to
already be in the content you post. The one exception is `canvas-reply`,
below — the card's script never fetches anything itself; it hands a value up
to the viewer, which makes the real HTTP call on its behalf.

### Interactive pages are artifacts

A prototype, an app, a game, anything someone clicks through is an artifact,
never a post. A card sizes itself to its content and takes Canvas's theme, so
a page that sizes itself from the viewport collapses in one. An artifact is a
folder of files Canvas keeps until someone deletes it, shown on the Artifacts
page in a pane with a real viewport, with its own colours and fonts untouched.
Link `/canvas.css` (above) for Canvas's tokens and base styles; its
`prefers-color-scheme` queries follow the theme the Canvas window shows.

```
canvas artifact new --title "Phone prototype"
# -> {"id":"art-3f9c2a1b7e","path":"/…/artifacts/art-3f9c2a1b7e",...}
canvas artifact put art-3f9c2a1b7e ./prototype/      # a folder's contents, or one file
canvas focus art-3f9c2a1b7e                          # switch the viewer to it
canvas artifact list | show <id> | delete <id>
```

- The pane opens `index.html`, else the folder's only top-level HTML file.
  Relative links, several pages, module scripts and the History API work.
- Declare the viewport the page is designed for with
  `<meta name="canvas-size" content="390x844">`; the pane opens at that size,
  clamped to the window. Without it the page fills the pane. The person can
  drag the pane to another size, kept per artifact, or fill the window with
  it. `canvas artifact pane <id> --size WxH|--reset|--full|--exit` does the
  same in every open viewer, and bare `canvas artifact pane` prints the frame
  size the viewer shows, so a page can be checked at a given width.
- A `put`, or a file saved into the folder, reaches every open page without
  the viewer rebuilding the pane. A change only to stylesheets the page links
  swaps them in place. Any other change reloads the page at the address it
  shows, hash and path included, so a page that routes by hash keeps its
  screen; reopening the artifact also returns there. A page that keeps state
  worth more than a reload handles the change itself:

  ```js
  window.addEventListener('canvas-artifact-changed', (e) => {
    // e.detail.paths: changed files relative to the folder, or null (unknown)
    if (e.detail.paths?.every((p) => p.startsWith('modules/'))) {
      e.preventDefault();               // no reload; apply it yourself
      for (const p of e.detail.paths) import(`./${p}?v=${Date.now()}`).then(swapIn);
    }
  });
  ```

  Keep code you want to swap this way in modules whose exports the page
  re-reads, and state in one object those modules don't own.
- When the person wants the files in their own repo, under git, link instead:
  `canvas artifact new --link ./web --title "Tower dash"` records that folder
  (or one `.html` file, served alone) rather than making one. Edit the files
  in place, never with `put`; a save reaches the open page as above. Delete
  keeps the files.
  If the folder moves, the pane says "Source missing" and `show` reports
  `"sourceMissing": true`; `canvas artifact relink <id> <new path>` repoints it
  and keeps the id.
- The page runs in `<iframe sandbox="allow-scripts">` with no
  `allow-same-origin`: scripts, styles and fonts load from its own folder,
  inline, or the three CDNs below; `connect-src` is closed and forms can't
  submit, so it can't fetch anything or reach Canvas.
- A page that throws reports it: while a viewer shows the artifact, `show`
  lists its newest 50 uncaught errors and unhandled rejections under
  `scriptErrors` (`at`, `kind`, `message`, `source`, `line`, `column`). Check
  it after a `put` or a save; an empty pane usually has one there. They live
  in memory only, so a daemon restart clears them. WebKit hides the message
  of an error thrown at the top level of an inline `<script>` or by an
  `onclick="…"` attribute; the entry says so, and moving that code into a
  `.js` file brings the message and line back.
- An artifact belongs to no session: any agent with its id can `put` to it,
  and it outlives the session that made it. Delete one whose job is done.
  `canvas artifact log <id>` prints which session and agent created, put,
  relinked or deleted it, and when a saved file changed it.

### Saving a page's state

An artifact page cannot use storage or `fetch`, so it keeps answers, scores
and progress by asking its parent, which hands them to Canvas. The same two
messages work for a managed and a linked artifact, and the page never learns
where the values live.

```js
// Save one value (any JSON; the viewer answers with an ack).
parent.postMessage({ type: 'canvas-state-set', key: 'quiz-3', value: { picked: 'b' } }, '*');
// Ask for everything saved; ask once, when the page loads.
parent.postMessage({ type: 'canvas-state-get' }, '*');

addEventListener('message', (e) => {
  if (e.source !== parent) return;
  if (e.data.type === 'canvas-state') {
    // e.data.values: { 'quiz-3': { picked: 'b' }, ... }, {} when nothing is saved
  } else if (e.data.type === 'canvas-state-ack') {
    // e.data: { key, ok: true } or { key, ok: false, error: 'why' }
  }
});
```

- `canvas-state` answers a `canvas-state-get` and nothing else. Canvas does not
  send it unasked, so a page that wants its saved values sends the get when
  it loads (and again after any change it wants to re-read).
- A key is a name, `^[a-z0-9][a-z0-9_-]{0,63}$`, not a path. Anything else gets
  an error ack and nothing is written. One key is one file,
  `<key>.json` in a `canvas-data/` folder.
- A value is any JSON, at most 256 KB per key and 5 MB per artifact. Over
  either cap the ack says so and nothing is written. A write replaces the
  whole key, atomically.
- Which artifact a message belongs to comes from which pane frame sent it,
  never from the message. Only the artifact open in the pane can save.
- Where it lands: a linked folder keeps `canvas-data/` inside the linked
  folder; a linked single `.html` file keeps it beside the file; a managed
  artifact keeps it inside its own folder (`<data dir>/artifacts/<id>/canvas-data/`).
  Canvas refuses to write when `canvas-data` or a key file is a symlink, or
  when `canvas-data` does not resolve to a folder directly inside the
  artifact's folder, so a page can never write anywhere else in a linked repo.
  Add `canvas-data/` to that repo's `.gitignore` unless the values belong in git.
- A save never reloads the page and never fires `canvas-artifact-changed`: the
  file watcher and the change stamp skip `canvas-data/`.
- `put` leaves the state alone, and skips a `canvas-data/` folder in what it
  copies. Deleting a managed artifact deletes its state; deleting a linked
  one leaves the files. Only `canvas artifact state <id> --clear` empties it.
- Agent side: `canvas artifact state <id>` prints every key as one JSON
  object, `canvas artifact state <id> <key>` prints one value (exit 1 when the
  key holds nothing), and `canvas artifact state <id> --clear` deletes every
  key and prints `{"cleared": N}`. For a linked artifact the files can also be
  read directly. The routes behind them are `GET|DELETE
  /api/artifacts/:id/state` and `GET|PUT /api/artifacts/:id/state/:key`.

### Asking a question in a card and waiting for the answer

A card's own script can send one value back to the session that posted it —
a button's answer, a form's value, anything JSON-serializable — by calling:

```js
parent.postMessage({ type: 'canvas-reply', value: 'A' }, '*');
```

The viewer relays it to canvasd, keyed by which card's iframe sent it — never
by an id the message names, so one card's script cannot write a reply onto
another card. The value is capped at 4KB and there's one reply per card
(a second `canvas-reply` overwrites the first); it lives in canvasd's memory
only, not in `stream.jsonl`, so it doesn't survive a daemon restart.

On the agent side:

```
canvas wait <card_id> [--timeout secs]   # blocks until a reply lands (default 30s), prints it as JSON
canvas card <card_id>                    # prints one post as JSON (id, sessionId, at, html, images, targets)
canvas replies <card_id>                 # one non-blocking check: prints the value and exits 0 if answered
```

`canvas card` also reads a post the user points at: their "Copy post link" menu item
produces `canvas-post://<card_id>`, so a pasted link means `canvas card <card_id>` — use the
`html` field to see what the post renders, and `canvas post --update <card_id>` to fix it.

`canvas replies` exits 1 for "no reply yet" — keep polling — and exits 3 with a message
on stderr if canvasd itself couldn't be reached, so a script polling on exit code alone
can tell "not answered" from "the daemon is down" instead of retrying forever.

Use `wait` when the next step genuinely depends on the answer and there's
nothing else to do meanwhile. Use `replies` when checking in between other
work — post a card with a question, keep going, and poll `replies` before
acting on the default the card describes.

### Pushing live data into a card

`--update` replaces a card's HTML, and the viewer builds a new iframe for it, so the
page loses its script state, form inputs and scroll. For a dashboard that changes while
you work, post the HTML once and push values into the running page:

```
canvas data <card_id> values.json    # or "-" for stdin; one JSON value, up to 256KB
```

The viewer delivers each value to that card's script as a message, without rebuilding
the iframe:

```js
addEventListener('message', (e) => {
  if (e.source === parent && e.data && e.data.type === 'canvas-data') render(e.data.value);
});
```

The latest value is kept (last write wins, in memory) and sent again whenever the
card's iframe reloads, so a theme change or an `--update` still shows current numbers;
the first paint of the page should handle having no value yet. A daemon restart clears
it, and the next `canvas data` refills it. `canvas data` exits non-zero with one line on
stderr for an unknown card id, a value that isn't JSON or one over 256KB.

### Artifact widgets and live data

Give an artifact a widget when its subject stays live for the length of a job: an
implementation run, a profiling loop, a diagnosis, a disk or service monitor, a project's
status. The widget shows in the artifact's row on the Artifacts page, under its title
and time; the Timeline shows nothing for it. Clicking the row opens the full page.

```
canvas artifact new --title "Disk monitor" --widget widget.html [--refresh '<cmd>' [--every <secs>]]
canvas artifact put <id> <file|dir> [--widget widget.html] [--refresh '<cmd>' [--every <secs>]]
canvas data <artifact_id> values.json      # or "-" for stdin
```

- `--widget <file>` is the widget's HTML, converted like a card body (by its extension,
  else Markdown). A `put` without `--widget` or `--refresh` keeps the ones the artifact
  has; with one, it replaces it.
- `--every` needs `--refresh`. A row without a widget shows only its title and time.
- Delete the artifact when the job ends. Don't make one for a one-off answer, a closing
  report or anything that won't change again: those belong in the feed.

**An artifact with a widget has two parts.** The page is an ordinary artifact and follows
the rules above. The widget is a separate small HTML document.

**The widget is about 196 px wide and as tall as its content, up to 120 px.** Canvas clips
anything past that and never scrolls it, and adds no padding: give the widget's body 6 px
top and bottom and 10 px left and right. Canvas draws the border and the rounded corners,
and a red dot in the top-right corner when a refresh fails; keep the top-right 16 × 16 px
clear for it.

Put one glanceable answer in the widget: the number or state someone checks this
artifact for.
- One of: a short status with a word label ("build 42 passing"); one big value (about
  18 px) with what it moved from; two or three counts, each with a word label; one or two
  bars, each with a label and a value; a sparkline beside a value.
- At most one line of muted detail at 11 px. The row's title already names the job; don't
  repeat it.

Do:
- Label every number and every colour with a word. Colour never carries the meaning alone
  ("1 missing" in red, not a red "1").
- Theme it like a card: a light palette on `:root`, overridden under
  `@media (prefers-color-scheme: dark)`. Don't set `html` or `body` background or color.
- Use the same numbers as the page, so opening it never contradicts the widget.
- Keep it under about 4 KB.

Don't:
- Put links, buttons or inputs in a widget. The whole row is one click target that
  opens the page; put links and controls there.
- Use images, external scripts or web fonts.
- Write prose. If the answer needs a sentence, it belongs in the page.
- Use font sizes under 10 px.

**Keeping it current.** Either push values yourself whenever you have new data, or let
canvasd pull them:

- `canvas data <artifact_id> <file|->` pushes one JSON value to the widget and the open
  page, the same channel as `canvas data <card_id>` (see above). An unknown id exits
  non-zero with one line on stderr.
- `--refresh '<cmd>' [--every <secs>]` makes canvasd run `<cmd>` under `sh -c` in the
  directory you ran `canvas artifact` from, every `--every` seconds (default 30, minimum
  5), while the agent that set it is running; a refresh set from a plain shell runs until
  the artifact is deleted. One run at a time; a run is killed after 60s. Use absolute paths
  in `<cmd>`: canvasd runs under launchd with a minimal `PATH`.
- The command prints one JSON value to stdout, not HTML. canvasd delivers it to the widget
  and the page as a `canvas-data` message, exactly as `canvas data` does. Both therefore
  carry a small script that redraws from that value (see "Pushing live data into a
  card"), and both handle having no value yet on first paint. The latest value is kept in
  memory and sent again whenever either frame loads.
- If the command exits non-zero, times out or prints something that isn't JSON, the last
  good value stays on screen, dimmed, with a red dot and "refresh failed" under the
  widget; the open page shows "Refresh failed 2m ago: <first stderr line or reason> ·
  retrying in 4m" under its header, and `canvas artifact show <id>` reports it as
  `refreshError` (`message`, `at`, `retryAt`). canvasd waits twice as long after each
  consecutive failure, up to 10 minutes. The next success clears the error.

### Result

`canvas post` is meant to be seen: if canvasd isn't running,
`CLAUDE_CODE_SESSION_ID` isn't set (it only runs inside a Claude Code
session), or the input is empty, it prints one line to stderr and exits
non-zero. On success it prints one JSON line to stdout —
`{"card_id": "...", "images": [...], "targets": [...]}` — listing the local
images and clickable targets it found. Check the exit code and read the
message rather than assuming the post landed.

## Adjusting guidance

The posting guidance a session receives is a named profile: the one assigned to the
session's GitHub repo, else the one assigned globally, else the built-in default. Change
the guidance for the repo you are working in:

```
canvas profile set terse notes.md     # create or replace a profile from a file (or "-" for stdin)
canvas profile assign terse --here    # use it for this directory's GitHub repo
canvas profile show --effective       # print the text this repo's sessions get
canvas profile unassign --here        # back to the global profile or the default
```

Other verbs: `canvas profile list` (profiles, the global and per-repo assignments, whether the
built-in default exists), `canvas profile show <name>`, `canvas profile delete <name>`,
`canvas profile assign <name>` (globally), and `--repo owner/name` in place of `--here` to
name a repo directly. `--kind <k>` selects the profile kind; it defaults to `posting-guidance`;
`stop-triggers` is the other kind. `--here` fails if the directory has no GitHub `origin`; every verb prints
one line on stderr and exits non-zero on any error.

A change applies to sessions that start after it: the SessionStart hook reads the effective
profile once, when the session starts.

### Post-reminder triggers

The prompt hook reminds a session to post when its previous turn ended with something worth
showing and no `canvas post`. Which turns count is the `stop-triggers` profile kind: one directive per line
(`image`, `file`, `report`, `verify`, `links [N]`, `long-block [N]`, `phrase <text>`,
`scratch <prefix>`, `no <directive>`, `off`, `on`), applied top to bottom. The same verbs
apply with `--kind stop-triggers`:

```
canvas profile show --kind stop-triggers --effective   # the directives in effect here
printf 'no links\nlong-block 30\n' | canvas profile set --kind stop-triggers quieter -
canvas profile assign --kind stop-triggers quieter --here
```

`canvas profile set` refuses text that doesn't parse and names the line.
