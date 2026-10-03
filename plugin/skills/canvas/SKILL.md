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
when the window can't show the card: pinned, its session archived or hidden,
or filtered out by the search (`canvas focus <card_id>` clears the last two).
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

### Pinned posts and widgets

Pin a post when its subject stays live for the length of a job: an implementation
run, a profiling loop, a diagnosis, a disk cleanup, a project's status. A pinned
post leaves the feed and shows as a small widget on the pin shelf under the
toolbar; the shelf exists only while something is pinned. Clicking the widget
opens the full post as a sheet over the feed.

```
canvas post card.html --pin <slot> [--pin-scope session|repo] [--widget widget.html]
canvas post card.html --pin <slot> --refresh '<cmd>' [--every <secs>]
canvas data --slot <slot> values.json      # or "-" for stdin
canvas unpin <slot|card_id>
```

- One slot holds one post per repo (per working directory when the session has no
  GitHub repo). Posting to a held slot again replaces that card in place, keeping its
  id and taking over its session, so you never track a card id.
- `--pin-scope` is `session` (the default) or `repo`. A session-scoped pin returns to the
  feed when its session ends or its agent process dies. A repo-scoped pin survives
  session end, session delete and clearing a session's cards.
- `canvas unpin <slot|card_id>` ends the job's pin: the card goes back to the feed at its
  own time. Run it when the job ends.
- `--widget <file>` is the widget's HTML, converted like a card body. Without it the shelf
  shows the card's first heading.
- `--pin-scope`, `--widget`, `--refresh` and `--every` need `--pin`; `--every` needs
  `--refresh`; `--pin` cannot be combined with `--update`.
- Don't pin a one-off answer, a closing report or anything that won't change again.
  Those belong in the feed.

**A pinned post has two parts.** The full post is an ordinary card and follows every
rule above. The widget is a separate small HTML document.

**The widget is 200 × 104 px.** Canvas clips anything outside it and never scrolls it, and
adds no padding: give the widget's body 8 px top and bottom and 10 px left and right, which
leaves 180 × 88 px of content. Canvas draws the border, the rounded corners, the session
colour dot and a short age (`12s`, `3m`) in the top-right corner; keep the top-right
40 × 14 px clear for them.

Put one glanceable answer in the widget: the number or state someone checks this pin for.
- Line 1 is a title of 24 characters or fewer, in bold 12 px, that names the job
  ("implement canvas-12", "p95 /search").
- Below it goes one of: a progress strip with the current step named in words; two to
  four counts, each with a word label under it; one big value (about 20 px) with what it
  moved from; two bars, each with a label and a value; a sparkline under a big value.
- At most one line of muted detail at 11 px.

Do:
- Label every number and every colour with a word. Colour never carries the meaning alone
  ("1 missing" in red, not a red "1").
- Theme it like a card: a light palette on `:root`, overridden under
  `@media (prefers-color-scheme: dark)`. Don't set `html` or `body` background or color.
- Use the same numbers as the full post, so opening it never contradicts the widget.
- Keep it under about 4 KB. It re-renders on every update.

Don't:
- Put links, buttons or inputs in a widget. The whole widget is one click target that
  opens the full post; put links and controls there.
- Use images, external scripts or web fonts.
- Write prose. If the answer needs a sentence, it belongs in the full post.
- Use font sizes under 10 px.
- Repeat the repo or session name. The full post's header already shows both.

**Keeping a pin current.** Refreshing is optional. Either push values yourself whenever
you have new data, or let the daemon pull them:

- `canvas data --slot <slot> <file|->` pushes one JSON value to the card in that slot, the
  same data channel as `canvas data <card_id>` (see above). An unknown slot exits non-zero
  with one line on stderr.
- `--refresh '<cmd>' [--every <secs>]` makes the daemon run `<cmd>` under `sh -c` in the
  session's working directory, every `--every` seconds (default 30, minimum 5), while the
  pin exists and its session is live. One run at a time; a run is killed after 60s. Use
  absolute paths in `<cmd>`: the daemon runs under launchd with a minimal `PATH`.
- The command prints one JSON value to stdout, not HTML. The daemon delivers it to the
  full post and the widget as a `canvas-data` message, exactly as `canvas data` does. Both
  the card and the widget therefore carry a small script that redraws from that value (see
  "Pushing live data into a card"), and both handle having no value yet on first paint.
- If the command exits non-zero, times out or prints something that isn't JSON, the last
  good value stays on screen, the first stderr line (or the reason) shows as an error mark
  on the widget and an error line in the sheet, and the daemon waits twice as long after
  each consecutive failure, up to 10 minutes. The next success clears the error.

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
