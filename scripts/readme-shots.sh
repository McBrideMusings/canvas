#!/usr/bin/env bash
# bash scripts/readme-shots.sh — regenerate the README screenshot in
# docs/screenshots/. Starts a throwaway daemon and dev app through
# scripts/verify-app.sh, posts a fixed set of made-up cards from made-up
# sessions, captures the window, and stops both processes. Nothing here
# touches the live daemon or names a real repo.
#
# Each session's cwd is a directory under /tmp/canvas-shots whose git
# `origin` points at an invented github.com repo, which is how canvasd names
# the session.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
root=/tmp/canvas-shots
out="${repo_root}/docs/screenshots"
verify="${repo_root}/scripts/verify-app.sh"

bash "${verify}" start
trap 'bash "${verify}" stop' EXIT
socket="$(bash "${verify}" env | sed 's/^export CANVAS_SOCKET=//')"
canvas="${repo_root}/target/debug/canvas"

rm -rf "${root}"
mkdir -p "${root}" "${out}"

# checkout <dir> <github owner/repo>
checkout() {
  mkdir -p "${root}/$1"
  git -C "${root}/$1" init -q
  git -C "${root}/$1" remote add origin "https://github.com/$2.git"
}

checkout docs-site octo-labs/docs-site
checkout storefront acme/storefront
checkout canvas acme/canvas
checkout billing-api acme/billing-api

# post <session id> <dir> <format> — the post body comes on stdin.
post() {
  (cd "${root}/$2" && CANVAS_SOCKET="${socket}" CLAUDE_CODE_SESSION_ID="$1" "${canvas}" post - --format "$3" > /dev/null)
}

# Oldest first: the stream shows the newest card on top.

post shot-docs docs-site md <<'MD'
## Docs navigation, before and after

| Page | Old section | New section |
| --- | --- | --- |
| Installing | Guides | Getting started |
| Webhooks | Reference | Guides |
| Rate limits | FAQ | Reference |
| Changelog | Footer | Top bar |

Four pages moved; no URLs changed, so no redirects are needed.
MD

post shot-storefront storefront md <<'MD'
## Checkout retry fix

The retry loop now stops on a 4xx instead of resubmitting the order:

```js
if (res.status < 500) {
  throw new Error(`order rejected: ${res.status}`);
}
await sleep(250 * attempt);
```

All 42 checkout tests pass.
MD

post shot-canvas canvas html <<'HTML'
<style>
  body { font: 14px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 18px 20px; color: #1f2328; background: #fff; }
  h2 { font-size: 17px; margin: 0 0 4px; }
  p { margin: 0 0 14px; color: #59636e; }
  ol { list-style: none; padding: 0; margin: 0; display: grid; gap: 8px; }
  li { display: grid; grid-template-columns: 22px 1fr auto; align-items: center; gap: 8px;
       padding: 8px 12px; border: 1px solid #d1d9e0; border-radius: 8px; }
  .mark { width: 16px; height: 16px; border-radius: 50%; border: 2px solid #8c959f; box-sizing: border-box; }
  .done .mark { background: #1a7f37; border-color: #1a7f37; }
  .now .mark { border-color: #bf8700; background: conic-gradient(#bf8700 0 50%, transparent 0); }
  .done span:nth-child(2) { color: #59636e; text-decoration: line-through; }
  .tag { font-size: 12px; color: #59636e; }
</style>
<h2>Plan: search across card text</h2>
<p>Five steps; the stream stays usable after each one.</p>
<ol>
  <li class="done"><span class="mark"></span><span>Index card text in canvasd as each post arrives</span><span class="tag">canvasd</span></li>
  <li class="done"><span class="mark"></span><span>Add <code>GET /api/search?q=</code></span><span class="tag">canvasd</span></li>
  <li class="now"><span class="mark"></span><span>Filter the stream from the toolbar search field</span><span class="tag">viewer</span></li>
  <li><span class="mark"></span><span>Highlight matches inside HTML cards</span><span class="tag">viewer</span></li>
  <li><span class="mark"></span><span><code>canvas search</code> for agents</span><span class="tag">cli</span></li>
</ol>
HTML

post shot-billing billing-api html <<'HTML'
<style>
  body { font: 14px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 18px 20px; color: #1f2328; background: #fff; }
  h2 { font-size: 17px; margin: 0 0 4px; }
  p { margin: 0 0 12px; color: #59636e; }
  svg { display: block; width: 100%; height: auto; }
  .grid { stroke: #d1d9e0; }
  .axis { fill: #59636e; font-size: 11px; }
  .before { fill: #d1d9e0; }
  .after { fill: #0969da; }
  .legend { font-size: 12px; fill: #1f2328; }
</style>
<h2>p95 latency after the invoice cache</h2>
<p>p95 fell from 180ms to 12ms on the four busiest endpoints. Load test: 500 req/s for 10 minutes.</p>
<svg viewBox="0 0 800 250" role="img" aria-label="Bar chart of p95 latency before and after the cache for four endpoints">
  <g class="grid">
    <line x1="60" x2="790" y1="20" y2="20"/><line x1="60" x2="790" y1="70" y2="70"/>
    <line x1="60" x2="790" y1="120" y2="120"/><line x1="60" x2="790" y1="170" y2="170"/>
    <line x1="60" x2="790" y1="220" y2="220"/>
  </g>
  <g class="axis" text-anchor="end">
    <text x="52" y="24">200ms</text><text x="52" y="74">150ms</text><text x="52" y="124">100ms</text>
    <text x="52" y="174">50ms</text><text x="52" y="224">0</text>
  </g>
  <!-- scale: 1ms = 1px, baseline y=220 -->
  <rect class="before" x="100" y="40" width="60" height="180" rx="3"/>
  <rect class="after" x="166" y="208" width="60" height="12" rx="3"/>
  <rect class="before" x="280" y="75" width="60" height="145" rx="3"/>
  <rect class="after" x="346" y="211" width="60" height="9" rx="3"/>
  <rect class="before" x="460" y="100" width="60" height="120" rx="3"/>
  <rect class="after" x="526" y="206" width="60" height="14" rx="3"/>
  <rect class="before" x="640" y="130" width="60" height="90" rx="3"/>
  <rect class="after" x="706" y="210" width="60" height="10" rx="3"/>
  <g class="axis" text-anchor="middle">
    <text x="163" y="240">GET /invoices</text><text x="343" y="240">GET /invoices/:id</text>
    <text x="523" y="240">GET /customers/:id</text><text x="703" y="240">POST /charges</text>
  </g>
  <g class="legend">
    <rect class="before" x="610" y="2" width="12" height="12" rx="2"/><text x="628" y="12">before</text>
    <rect class="after" x="690" y="2" width="12" height="12" rx="2"/><text x="708" y="12">after</text>
  </g>
</svg>
HTML

sleep 2
bash "${verify}" shot "${out}/stream.png"
