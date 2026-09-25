---
name: canvas
description: Post a self-contained HTML, Markdown or text card to Canvas, the live stream of Claude Code sessions on this Mac. Post generously and deliberately for anything that reads better rendered than as chat Markdown — a file you created or changed, a screenshot, a chart or table, a diagram of a structure, more than ~15 lines of code or output, or several links worth clicking. Skip one-line status updates and anything that would just repeat the chat text.
---

# Canvas

Canvas is a stream of cards, one per `canvas post` call, shown in a viewer
that sits open beside the terminal. Each session gets its own column of
cards. Nothing creates a card automatically — a card exists only because a
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

Input is Markdown, plain text, or HTML — pick with `--format md|text|html`,
or let it infer from the file's extension (`.md`/`.markdown`, `.txt`,
`.html`/`.htm`); stdin or an unrecognised extension defaults to Markdown.
Markdown renders with tables and strikethrough, and raw HTML embedded in the
Markdown source passes through unchanged. Text renders as an HTML-escaped
`<pre>` block. HTML passes through as-is and must be self-contained: inline
styles or a `<style>` block, no external stylesheet.

### Local paths and links

An image at an absolute path that exists — `![](/abs/shot.png)` in Markdown
or `<img src="/abs/shot.png">` in HTML — renders inline in the card. A link
to an existing local file or to any `http://`/`https://` URL —
`[plan](/abs/plan.md)` or `<a href="…">` — opens on click. An absolute local
path that doesn't exist is left unchanged (a broken image or a dead link) and
`canvas post` warns about it on stderr — the post still lands.

### Sandbox limits

Posted HTML renders in `<iframe sandbox="allow-scripts">` (no
`allow-same-origin`) with a CSP that allows scripts only from
`cdnjs.cloudflare.com`, `cdn.jsdelivr.net`, and `unpkg.com` — pin a library
to an exact version from one of those. There is no `connect-src`, so the
page cannot `fetch()` or open a `WebSocket`; everything it shows has to
already be in the content you post.

### Result

`canvas post` is meant to be seen: if canvasd isn't running,
`CLAUDE_CODE_SESSION_ID` isn't set (it only runs inside a Claude Code
session), or the input is empty, it prints one line to stderr and exits
non-zero. On success it prints one JSON line to stdout —
`{"card_id": "...", "images": [...], "targets": [...]}` — listing the local
images and clickable targets it found. Check the exit code and read the
message rather than assuming the post landed.
