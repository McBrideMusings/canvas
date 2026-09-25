# Canvas

One live stream of posts from every Claude Code session on this Mac.

## Language

### Domain

**Session**:
One main Claude Code conversation on this Mac, from SessionStart to SessionEnd. It is active until it ends. An ended session is hidden by default; its cards are kept and come back with Show in Settings.
_Avoid_: agent, conversation

**Hidden session**:
A session whose chip and cards the viewer leaves out, because it ended while Hide sessions when they end is on, or because someone chose Hide this session. Settings lists it with Show and Delete. Hiding is per viewer and changes nothing in canvasd.
_Avoid_: archived, closed

**Card**:
One post from one session, shown in the stream. Images in it render where the post puts them and open full size in a lightbox on click; local file paths and URLs written as links open on click.
_Avoid_: turn card, post (for the rendered card)

**Post**:
HTML, Markdown or plain text an agent sends with `canvas post`. Each post becomes one card.
_Avoid_: explicit post, surface

**Stream**:
Every card from the selected sessions, newest first. "All" sessions is the default selection.
_Avoid_: timeline, feed

### Architecture

{Seeded on first run of `improve`. Don't seed up front.}

## Relationships

{Filled in as the model matures.}

## Flagged ambiguities

{When terms get used ambiguously and resolved, capture here.}
