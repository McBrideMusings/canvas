# Canvas

One live Timeline of posts from every Claude Code session on this Mac, and the artifacts they keep.

## Language

### Domain

**Session**:
One main agent conversation on this Mac, from its first post to SessionEnd, started by one Agent. A conversation that never posts has no session. It is active until it ends. An ended session is archived by default; its cards are kept and come back with Show in the Sessions drawer.
_Avoid_: conversation

**Agent**:
The coding tool that started a session: Claude Code or Codex. canvasd records it from the session's first post and never changes it afterward.
_Avoid_: source, client

**Archived session**:
A session whose chip and cards the viewer leaves out. A session archived because it ended, while Archive sessions when they end is on, keeps its cards, and Show brings them back. Archive in the Sessions drawer also deletes the session's cards from canvasd. The drawer lists archived sessions under Archived, each with Show and Delete.
_Avoid_: hidden, closed

**Sessions drawer**:
The column that docks beside the Timeline, narrowing it, and lists every session under Active and Archived. An active row has an eye toggle (show or hide its cards) and Archive. An archived row has Show and Delete.
_Avoid_: sidebar, session list

**Card**:
One post from one session, shown on the Timeline. Images in it render where the post puts them and open full size in a lightbox on click; local file paths and URLs written as links open on click.
_Avoid_: turn card, post (for the rendered card)

**Post**:
HTML, Markdown or plain text an agent sends with `canvas post`. Each post becomes one card.
_Avoid_: explicit post, surface

**Card data**:
One JSON value an agent pushes into a running card or artifact with `canvas data`. canvasd keeps the latest, up to 256 KB, in memory only, and the viewer hands it to the page without rebuilding it. An artifact's refresh command produces it on a timer.
_Avoid_: payload, stream, live update

**Chip**:
One session's button in the row under the title bar: its agent icon on a tile in the session colour, its name, its repo and its card count. Clicking a chip adds its session to the visible set, or removes it. With nothing chosen, every session shows; the drawer's eye toggles change the same set.
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

**Export**:
A post saved as one standalone HTML page with its images, scripts, styles and fonts inlined, so it opens without Canvas or the network. Exporting every post or one session produces a zip of such pages plus an index.
_Avoid_: download, save, backup

**Instructions**:
The text an agent reads at session start about when to post to Canvas and how to build a post. It has up to three layers, read in this order: built-in (ships in `plugin/`, read-only, with an Include switch), yours (one per Mac), and the project's (`.canvas/instructions.md` at the repo's git root). Later layers win where they differ.
_Avoid_: guidance, posting-guidance profile

### Architecture

{Seeded on first run of `improve`. Don't seed up front.}

## Relationships

{Filled in as the model matures.}

## Flagged ambiguities

{When terms get used ambiguously and resolved, capture here.}
