# Session and turn stream

canvasd tracks sessions in memory and holds the newest 500 turn cards in a ring buffer,
pushing every change over SSE to connected viewers.

## Sub-features

- `session-upsert` — `POST /api/sessions` creates or updates a session, computing `name`
  as the `cwd` basename.
- `session-end` — `POST /api/sessions/:id/end` sets `endedAt`.
- `turn-open-close` — `POST /api/turns` extends and closes a session's open card, or
  creates a new closed card, or (with an empty payload and no open card) does nothing.
- `ring-eviction` — the 501st card evicts the oldest.
- `state-snapshot` — `GET /api/state` returns `{sessions, cards}`, cards newest first.
- `sse-stream` — `GET /api/events` emits `session-upserted` and `card-upserted`.

## How to get to it (user POV)

Nothing here is user-facing directly — it's the API that hooks (a later slice) and the
viewer call. A person only sees its effect as cards appearing in the viewer stream.

## Driving it with curl

Preconditions: canvasd running on a spare port (see SKILL.md Launch).

- **Create a session** — `curl -s -X POST http://127.0.0.1:8231/api/sessions -H
  'content-type: application/json' -d
  '{"sessionId":"s1","cwd":"/Users/me/Projects/canvas","claudePid":111}'` → JSON
  session with `"name":"canvas"`.
- **End it** — `curl -s -X POST http://127.0.0.1:8231/api/sessions/s1/end` → `endedAt`
  is now set in a follow-up `GET /api/state`.
- **Empty turn, no open card** — `curl -s -X POST http://127.0.0.1:8231/api/turns -H
  'content-type: application/json' -d
  '{"sessionId":"s1","links":[],"paths":[],"images":[]}'` then `GET /api/state` → `cards`
  is still empty.
- **Turn with content, no open card** — same call with a non-empty `links` array →
  `GET /api/state` shows one new card with `"open":false`.
- **Ring eviction** — loop `POST /api/turns` 501 times with a distinct link each time
  (see `cargo test` name `ring_evicts_oldest_at_501` for the exact loop) → `GET
  /api/state` shows exactly 500 cards and the very first link is gone.
- **SSE** — `curl -N http://127.0.0.1:8231/api/events` in one terminal, then fire any of
  the calls above in another → an `event: card-upserted` or `event: session-upserted`
  block appears with matching JSON data.

## Gotchas

- The ring buffer is per-process and in-memory — restarting canvasd during a drive
  silently resets it to empty. `GET /api/state` before and after any restart to confirm
  which instance you're talking to.
- `session-upserted`/`card-upserted` SSE payloads are the full object, not a diff —
  don't expect a partial patch.
