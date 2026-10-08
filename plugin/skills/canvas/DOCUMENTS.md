# Building a card as a document

A card that earns its post (see the test in the instructions) is a page the user reads,
not a note. This file is how to build one. Everything stays in one self-contained
HTML file posted with `canvas post file.html`.

## Shape

1. **A one-sentence lead** under the title: what this page shows and the answer.
2. **Numbered panels**, each an `<h2>` that is a full sentence (subject, verb,
   object). Reading only the `<h2>`s top to bottom must tell the whole story;
   check this before you post.
3. **One figure per panel**: a diagram, a chart, a table or an image, with the
   prose beside or below it, never instead of it.
4. **A closing line** with the one thing to remember or the decision needed.

Cite where a claim comes from: a `file:line` in a `<code>` tag, a command you ran,
a screenshot. Mark a judgement you did not check as a guess in the text.

## Drawing the structure

Draw a flow, call chain, layout or state machine as inline SVG, not as an indented
list. Use `currentColor` for every stroke and label so the theme sets the ink, and
one accent color for the one thing the panel is about; a failure or gotcha is a
dashed outline plus a label, never a second hue.

```html
<svg viewBox="0 0 640 120" width="100%" role="img" aria-label="hook to daemon to card">
  <defs>
    <marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto">
      <path d="M0 0L10 5L0 10z" fill="currentColor"/>
    </marker>
  </defs>
  <g fill="none" stroke="currentColor" stroke-width="1.5">
    <rect x="10"  y="40" width="150" height="40" rx="6"/>
    <rect x="245" y="40" width="150" height="40" rx="6" stroke="var(--accent)" stroke-width="2.5"/>
    <rect x="480" y="40" width="150" height="40" rx="6" stroke-dasharray="5 4"/>
    <path d="M160 60H243" marker-end="url(#a)"/>
    <path d="M395 60H478" marker-end="url(#a)"/>
  </g>
  <g fill="currentColor" font-size="14" text-anchor="middle">
    <text x="85"  y="65">canvas post</text>
    <text x="320" y="65">canvasd</text>
    <text x="555" y="65">viewer</text>
  </g>
</svg>
```

Give every arrow its own `<path>`: `marker-end` draws a head only at the end of
the last subpath. Space boxes on a grid, keep labels under about 20 characters,
and put an arrow's meaning in a small label beside it, clear of the line. Use a table, not SVG, for anything that is a
list of like things with attributes.

## Skeleton

```html
<!doctype html>
<title>Short name</title>
<style>
  :root { --accent: #b3261e; --line: #c9c9c9; --soft: #f4f4f4; --soft-ink: #222; }
  @media (prefers-color-scheme: dark) {
    :root { --accent: #ff8a80; --line: #444; --soft: #26262a; --soft-ink: #eee; }
  }
  body { font: 15px/1.5 system-ui, sans-serif; margin: 0; padding: 20px; }
  .lead { font-size: 17px; margin: 0 0 20px; }
  section { border-top: 1px solid var(--line); padding: 18px 0; }
  h2 { font-size: 16px; margin: 0 0 10px; }
  h2 .n { color: var(--accent); margin-right: 8px; }
  table { border-collapse: collapse; width: 100%; }
  th, td { border-bottom: 1px solid var(--line); padding: 6px 8px; text-align: left; }
  th { background: var(--soft); color: var(--soft-ink); }
  .note { background: var(--soft); color: var(--soft-ink); padding: 10px 12px; border-radius: 6px; }
  code { font-size: 13px; }
</style>
<h1>Short name</h1>
<p class="lead">What this shows, and the answer, in one sentence.</p>
<section><h2><span class="n">1</span>The hook sends each post to the daemon.</h2>
  <!-- figure -->
  <p>Prose beside the figure, with <code>/abs/path/file.rs:42</code> for the claim.</p>
</section>
<section><h2><span class="n">2</span>The daemon keeps the newest 500 posts.</h2>
  <!-- table or chart -->
</section>
<p class="note">The one thing to remember, or the decision needed.</p>
```

## Check before posting

- Read the `<h2>`s alone: do they tell the story?
- Every panel has a figure, table or image, not only paragraphs.
- Open it in both themes: ink and boxes stay readable, no hard-coded page
  background or text color.
- Post it, then look at the card in Canvas and fix what is cramped or clipped
  with `canvas post --update <card_id>`.
