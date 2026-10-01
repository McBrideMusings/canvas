---
name: canvas
description: Post a self-contained HTML, Markdown or text card to Canvas, the live stream of Claude Code sessions on this Mac. Post generously and deliberately for anything that reads better rendered than as chat Markdown — a file you created or changed, a screenshot, a chart or table, a diagram of a structure, more than ~15 lines of code or output, or several links worth clicking. Skip one-line status updates and anything that would just repeat the chat text.
---

# Canvas

Canvas is a stream of cards, one per `canvas post` call, shown in a viewer
that sits open beside the terminal. Cards from every session share one
stream, newest first; a chip per session filters it. Nothing creates a card automatically — a card exists only because a
post created it.

## How to post

Run `canvas post` with the content as a file argument, or piped in on
stdin:

```
canvas post /path/to/plan.md
canvas post /path/to/report.html --format html
echo 'done' | canvas post
canvas post - <<'EOF'
## Plan
- Step one
- Step two
EOF
```

`--update <card_id>` replaces an existing card's content in place instead of
creating a new one — the `card_id` a prior `canvas post` reported in its
JSON output. Use it for a card that represents one ongoing unit of work (a
long task's status, a running checklist) rather than posting a fresh card
every time it changes — the viewer updates the card where it sits, in every
open window, without disturbing its scroll position or any other card. There
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

Input is Markdown, plain text, or HTML — pick with `--format md|text|html`,
or let it infer from the file's extension (`.md`/`.markdown`, `.txt`,
`.html`/`.htm`); stdin or an unrecognised extension defaults to Markdown.
Markdown renders with tables and strikethrough, and raw HTML embedded in the
Markdown source passes through unchanged. Text renders as an HTML-escaped
`<pre>` block. HTML passes through as-is and must be self-contained: inline
styles or a `<style>` block, no external stylesheet.

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
an image inside a link to a URL, or beside other text, opens the link. Only those two forms are picked up: a bare path in the text
stays plain text. An absolute local path that doesn't exist is left
unchanged (a broken image or a dead link) and `canvas post` warns about it on
stderr — the post still lands.

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
