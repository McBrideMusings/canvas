# Canvas

One live Timeline of posts from every Claude Code session on this Mac, and the artifacts they keep.

## Language

### Domain

**Session**:
One main agent conversation on this Mac, from its first post to SessionEnd, started by one Agent. A conversation that never posts has no session. It is active until it ends. An ended session is archived by default; its cards are kept and come back with Show in the Sessions drawer.
_Avoid_: conversation

**Agent**:
The coding tool that started a session. Claude Code is the only one today; the daemon records it on the session when the session is registered and never changes it afterward.
_Avoid_: source, client

**Archived session**:
A session whose chip and cards the viewer leaves out, because it ended while Archive sessions when they end is on, or because someone chose Archive this session. The Sessions drawer lists it under Archived with Show and Delete. Archiving is per viewer and changes nothing in canvasd.
_Avoid_: hidden, closed

**Sessions drawer**:
The panel that slides in from the right edge and lists every session under Active and Archived. Active rows can have their posts cleared or be archived; archived rows can be shown again or deleted, one at a time or all at once.
_Avoid_: sidebar, session list

**Card**:
One post from one session, shown on the Timeline. Images in it render where the post puts them and open full size in a lightbox on click; local file paths and URLs written as links open on click.
_Avoid_: turn card, post (for the rendered card)

**Post**:
HTML, Markdown or plain text an agent sends with `canvas post`. Each post becomes one card.
_Avoid_: explicit post, surface

**Chip**:
One session's button in the row under the title bar: its agent icon on a tile in the session colour, its name, its repo and its card count. Clicking it shows only that session's cards.
_Avoid_: tab, pill, sidebar row

**Session colour**:
The colour a session's chip tile, card header tile and arrival ring share. Handed out in order of first appearance, per repo by default or per session in Settings.
_Avoid_: hue, accent

**Timeline**:
The window's first page: every card that matches the selected chip and the search and belongs to no archived session, most recently posted or updated first. All is the default chip. The search field and chips show only on it.
_Avoid_: stream, feed

**Artifact**:
A web page an agent keeps in Canvas until someone deletes it, with an id starting `art-`: a folder of files canvasd owns under `artifacts/<id>/`, or a folder or HTML file the person owns that it links to. It belongs to no session, runs in its own viewport, and Canvas never themes it. Every interactive page is an artifact; a post never is.
_Avoid_: app post, pinned card

**Artifacts page**:
The window's second page: every artifact, most recently changed first, in a list beside the open artifact's pane. The pane gives the page its declared `canvas-size`, clamped to the window, else the whole pane.
_Avoid_: gallery, library

### Architecture

{Seeded on first run of `improve`. Don't seed up front.}

## Relationships

{Filled in as the model matures.}

## Flagged ambiguities

{When terms get used ambiguously and resolved, capture here.}
