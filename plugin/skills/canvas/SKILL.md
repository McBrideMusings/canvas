---
name: canvas
description: Post a self-contained HTML card to Canvas, the live stream of Claude Code sessions on this Mac. Use when a turn produces a plan, a design, or a structured explanation worth more than plain chat text.
---

# Canvas

Canvas is a stream of cards, one per turn, shown in a viewer that sits open
beside the terminal. Each session gets its own column of cards; a card can
carry links, file paths, images, and posted HTML.

## What to post

Post plans, designs, and structured explanations — anything a diagram, a
layout, or formatted HTML would make clearer than a wall of chat text — as
self-contained HTML at the end of the turn that produced them. A short answer
or a one-line status update doesn't need a post.

**Never post links, file paths, or images by hand.** The `Stop` hook already
pulls those out of the turn's own text and tool calls and attaches them to
the same card automatically. Posting a bare URL or path as HTML just
duplicates what's about to appear anyway.

## How to post

Run `canvas post` with the HTML as a file argument, or piped in on stdin:

```
canvas post /path/to/plan.html
echo '<p>done</p>' | canvas post
canvas post - <<'EOF'
<h2>Plan</h2>
<ul><li>Step one</li><li>Step two</li></ul>
EOF
```

The HTML must be self-contained: inline styles or a `<style>` block, no
external stylesheet. It renders in a sandboxed iframe
(`sandbox="allow-scripts"`, no `allow-same-origin`) with a CSP that allows
scripts only from `cdnjs.cloudflare.com`, `cdn.jsdelivr.net`, and
`unpkg.com` — pin a library to an exact version from one of those. There is
no `connect-src`, so the page cannot `fetch()` or open a `WebSocket`;
everything it shows has to already be in the HTML you post.

Unlike a hook, `canvas post` is meant to be seen: if canvasd isn't running,
`CLAUDE_CODE_SESSION_ID` isn't set (it only runs inside a Claude Code
session), or the HTML is empty, it prints one line to stderr and exits
non-zero. Check the exit code and read the message rather than assuming the
post landed.
