#!/usr/bin/env bash
# admin seed <socket> — load a fixed fixture of sessions and posts into the
# canvas daemon listening on the Unix socket at <socket>. The live daemon's
# socket (canvasd.sock in ~/Library/Application Support/canvas) is refused, since the clear step below would delete that
# stream's seed-* sessions.
#
# It clears first: every fixture session (ids seed-*) is deleted, taking its
# cards with it, then recreated, so a second run leaves the same sessions and
# cards as the first. Sessions without the seed- prefix are left alone.
#
# Posts go through `canvas post` (the debug build), so Markdown gets the same
# conversion, default style, and path/image scan a real agent's post gets.
# Each session's cwd is a directory under /tmp/canvas-seed, rebuilt on every
# run; its git `origin` is a github.com URL, which is how canvasd names the
# session's repo.
set -euo pipefail

socket="${1:?usage: admin seed <socket>}"
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
root=/tmp/canvas-seed
ids=(seed-storefront seed-billing seed-docs seed-scratch seed-legacy)

live="${HOME}/Library/Application Support/canvas/canvasd.sock"
if [ "${socket}" = "${live}" ]; then
  echo "seed: refusing ${live}, the live daemon's socket; seed a spare daemon" >&2
  exit 1
fi

api() { curl -s --unix-socket "${socket}" "$@"; }

if ! api -o /dev/null --max-time 2 "http://localhost/api/state"; then
  echo "seed: no canvas daemon answering on ${socket}" >&2
  exit 1
fi

cargo build -q --manifest-path "${repo_root}/Cargo.toml" -p canvas
canvas="${repo_root}/target/debug/canvas"

for id in "${ids[@]}"; do
  api -o /dev/null -X DELETE "http://localhost/api/sessions/${id}"
done

rm -rf "${root}"
mkdir -p "${root}"

# checkout <dir> [github owner/repo] — a directory, optionally a git checkout
# whose origin points at that GitHub repo.
checkout() {
  mkdir -p "${root}/$1"
  if [ -n "${2:-}" ]; then
    git -C "${root}/$1" init -q
    git -C "${root}/$1" remote add origin "https://github.com/$2.git"
  fi
}

checkout storefront acme/storefront
checkout billing-api acme/billing-api
checkout docs-site octo-labs/docs-site
checkout scratch
checkout legacy-cli acme/legacy-cli

printf '# billing-api\n\nFixture file for the seed path link.\n' > "${root}/billing-api/README.md"

# A 320x180 PNG gradient, written with the standard library only.
python3 - "${root}/billing-api/latency.png" <<'PY'
import struct, sys, zlib
w, h = 320, 180
rows = b"".join(
    b"\x00" + b"".join(bytes((40 + x * 180 // w, 90 + y * 120 // h, 200)) for x in range(w))
    for y in range(h)
)
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
open(sys.argv[1], "wb").write(png)
PY

# post <session id> <dir> <format> — the post body comes on stdin.
post() {
  (cd "${root}/$2" && CANVAS_SOCKET="${socket}" CLAUDE_CODE_SESSION_ID="$1" "${canvas}" post - --format "$3" > /dev/null)
}

post seed-legacy legacy-cli md <<'MD'
## Release checklist

The 2.x line is frozen. Last tag cut: `v2.9.4`.
MD
api -o /dev/null -X POST "http://localhost/api/sessions/seed-legacy/end"

post seed-scratch scratch text <<'TXT'
Scratch notes — no repo here.
Plain text keeps its own line breaks.
TXT

post seed-docs docs-site html <<'HTML'
<style>
  body { background: #16181d; color: #e8e6e1; padding: 16px; margin: 0; }
  h2 { color: #9ecbff; margin-top: 0; }
  .muted { color: #9a9ca5; }
</style>
<h2>Dark theme preview</h2>
<p>This post sets its own dark body background and light text.</p>
<p class="muted">Contrast ratio: 13.9:1</p>
HTML

post seed-billing billing-api md <<MD
## p95 latency after the cache change

![latency chart](${root}/billing-api/latency.png)

p95 dropped from 180ms to 12ms.
MD

post seed-billing billing-api md <<MD
## Where to look

- The service notes: [README.md](${root}/billing-api/README.md)
- Upstream docs: [Stripe API reference](https://stripe.com/docs/api)
MD

post seed-storefront storefront md <<'MD'
## Checkout form retry loop

The retry wraps the whole submit, so it runs once per failed attempt:

```js
async function submitOrder(form) {
  const body = new FormData(form);
  let attempt = 0;
  while (attempt < 3) {
    attempt += 1;
    const res = await fetch("/api/orders", { method: "POST", body });
    if (res.ok) {
      return res.json();
    }
    if (res.status < 500) {
      throw new Error(`order rejected: ${res.status}`);
    }
    await new Promise((r) => setTimeout(r, 250 * attempt));
  }
  throw new Error("order failed after 3 attempts");
}
```
MD

post seed-storefront storefront md <<'MD'
## Button styles

The primary button now uses the accent token:

```css
.btn-primary {
  background: var(--accent);
  color: #fff;
  border-radius: 6px;
}
```
MD

echo "seed: ${#ids[@]} sessions, 7 posts on ${socket} (fixture dirs in ${root})"
