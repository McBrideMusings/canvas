# Canvas

One live stream of links, file paths, images and HTML posts from every Claude Code session on this Mac.

## Language

### Domain

**Session**:
One main Claude Code conversation on this Mac, from SessionStart to SessionEnd. It is active until it ends; ended sessions stay listed while their cards remain.
_Avoid_: agent, conversation

**Turn card**:
Everything one turn of one session produced: explicit posts, images, links and file paths, shown as one card. A turn that produced none of these has no card.
_Avoid_: post (for the whole card), snippet

**Explicit post**:
HTML an agent sends on purpose with `canvas post`. It joins its session's current turn card.
_Avoid_: surface

**Stream**:
Every turn card from the selected sessions, newest first. "All" sessions is the default selection.
_Avoid_: timeline, feed

### Architecture

{Seeded on first run of `improve`. Don't seed up front.}

## Relationships

{Filled in as the model matures.}

## Flagged ambiguities

{When terms get used ambiguously and resolved, capture here.}
