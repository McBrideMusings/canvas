# Explicit posts

`canvas post` (a later slice's CLI) will call `POST /api/posts` with the agent's own
Claude process id and an HTML string. canvasd maps that pid to whichever registered
session is still open and appends the HTML to that session's open card, opening one if
none exists.

## Sub-features

- `pid-to-session` — resolves `claudePid` to the session that registered it and hasn't
  ended; 404 if none matches.
- `post-opens-card` — first post on a session with no open card creates one, `open:true`.
- `post-extends-card` — a second post before any turn closes it appends to the same
  card's `html` array.
- `turn-closes-post-card` — the next `/api/turns` call (even with empty lists) closes the
  card the posts opened.

## How to get to it (user POV)

Not user-facing yet — no `canvas post` CLI exists in this slice. Drive the HTTP endpoint
directly.

## Driving it with curl

Preconditions: a session registered with a known `claudePid` (see
`session-and-turn-stream.md`).

- **Unknown pid** — `curl -s -o /dev/null -w '%{http_code}' -X POST
  http://127.0.0.1:8231/api/posts -H 'content-type: application/json' -d
  '{"claudePid":99999,"html":"<p>x</p>"}'` → `404`.
- **Opens a card** — `curl -s -X POST http://127.0.0.1:8231/api/posts -H
  'content-type: application/json' -d
  '{"claudePid":111,"html":"<p>hello</p>"}'` → returned card has `"open":true` and
  `"html":["<p>hello</p>"]`.
- **Extends it** — repeat with different html → same card `id`, `html` now has both
  strings.
- **Turn closes it** — `POST /api/turns` for the same session with empty lists → `GET
  /api/state` shows that card now `"open":false`, `html` unchanged.

## Gotchas

- A session must be registered (via `/api/sessions`) *before* posting — the pid lookup
  only checks sessions canvasd already knows about, and only unended ones.
- Two sessions can't share a `claudePid` and both be open at once in this implementation
  — canvasd picks whichever unended session matches first. Don't register two open
  sessions with the same pid in a single drive.
