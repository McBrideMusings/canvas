#!/usr/bin/env bash
# bash scripts/clip-demo.sh — the demo Canvas plays for a recorded clip (admin's
# `kind = "clip"`). It is the launch command: it starts a throwaway daemon and dev
# app through scripts/verify-app.sh (own socket under /tmp/canvas-verify, empty data
# dir, so no real post can appear), puts the window on a Retina screen, hides the
# update banner, then plays a fixed choreography with `canvas post` and repeats it
# forever. It never takes the mouse, keyboard or focus.
#
# Clip contract:
#   ADMIN_CLIP=1          wait for the file named by ADMIN_CLIP_START, then start
#                         the timeline (that moment is t0); without it, start at once
#   period = 5.6 s        the stream is empty at t0 and again at t0 + period, so the
#                         frame at t0 + period equals the frame at t0
#
# Timeline, seconds after t0 (four made-up sessions, four cards, newest on top):
#   0.7 diagram  1.7 chart  2.7 screenshot  3.7 markdown plan
#   4.9 every demo session deleted, so the stream is empty again
#
# Needs: rsvg-convert (brew install librsvg) for the mock screenshot.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
verify="${repo_root}/scripts/verify-app.sh"
work=/tmp/canvas-clip-demo
canvas="${repo_root}/target/debug/canvas"
PERIOD=5.6
sessions=(demo-storefront demo-billing demo-web demo-docs)

rm -rf "${work}"
mkdir -p "${work}"

# The dev app opens on whichever screen it likes, often a 1x one, where a window
# records at half the pixels. Put it on the Retina screen (logical point x,y).
export CANVAS_DEBUG_WINDOW="${CLIP_DEMO_WINDOW:-300,200}"
bash "${verify}" start
trap 'bash "${verify}" stop >/dev/null' EXIT
socket="$(bash "${verify}" env | sed 's/^export CANVAS_SOCKET=//')"

# --- demo content ---------------------------------------------------------

# checkout <dir> <github owner/repo> — names the session in the stream.
checkout() {
  mkdir -p "${work}/cwd/$1"
  git -C "${work}/cwd/$1" init -q
  git -C "${work}/cwd/$1" remote add origin "https://github.com/$2.git"
}
checkout storefront acme/storefront
checkout billing-api acme/billing-api
checkout web acme/web
checkout docs-site octo-labs/docs-site

cat > "${work}/shot.svg" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="560" viewBox="0 0 1200 560" font-family="Helvetica, Arial, sans-serif">
  <rect width="1200" height="560" fill="#f6f8fa"/>
  <rect width="1200" height="64" fill="#ffffff"/><line x1="0" x2="1200" y1="64" y2="64" stroke="#d1d9e0"/>
  <circle cx="40" cy="32" r="12" fill="#0969da"/><text x="64" y="40" font-size="22" font-weight="700" fill="#1f2328">Acme Checkout</text>
  <text x="1020" y="40" font-size="18" fill="#59636e">Cart (3)</text>
  <rect x="60" y="100" width="700" height="400" rx="14" fill="#ffffff" stroke="#d1d9e0"/>
  <text x="90" y="150" font-size="26" font-weight="700" fill="#1f2328">Payment</text>
  <rect x="90" y="176" width="640" height="56" rx="8" fill="#ffffff" stroke="#8c959f"/>
  <text x="110" y="212" font-size="20" fill="#1f2328">4242 4242 4242 4242</text>
  <rect x="90" y="252" width="300" height="56" rx="8" fill="#ffffff" stroke="#8c959f"/><text x="110" y="288" font-size="20" fill="#1f2328">09 / 28</text>
  <rect x="430" y="252" width="300" height="56" rx="8" fill="#ffffff" stroke="#8c959f"/><text x="450" y="288" font-size="20" fill="#1f2328">CVC  •••</text>
  <rect x="90" y="340" width="640" height="64" rx="10" fill="#1a7f37"/>
  <text x="410" y="381" font-size="24" font-weight="700" fill="#ffffff" text-anchor="middle">Pay $84.00</text>
  <text x="90" y="450" font-size="18" fill="#1a7f37">✓ Retry stops on a rejected card instead of resubmitting</text>
  <rect x="800" y="100" width="340" height="400" rx="14" fill="#ffffff" stroke="#d1d9e0"/>
  <text x="830" y="150" font-size="24" font-weight="700" fill="#1f2328">Order summary</text>
  <text x="830" y="200" font-size="19" fill="#59636e">Canvas tote</text><text x="1110" y="200" font-size="19" fill="#1f2328" text-anchor="end">$32.00</text>
  <text x="830" y="240" font-size="19" fill="#59636e">Ink set</text><text x="1110" y="240" font-size="19" fill="#1f2328" text-anchor="end">$46.00</text>
  <text x="830" y="280" font-size="19" fill="#59636e">Shipping</text><text x="1110" y="280" font-size="19" fill="#1f2328" text-anchor="end">$6.00</text>
  <line x1="830" x2="1110" y1="310" y2="310" stroke="#d1d9e0"/>
  <text x="830" y="350" font-size="22" font-weight="700" fill="#1f2328">Total</text><text x="1110" y="350" font-size="22" font-weight="700" fill="#1f2328" text-anchor="end">$84.00</text>
</svg>
SVG
rsvg-convert -w 1200 "${work}/shot.svg" -o "${work}/checkout.png"

cat > "${work}/diagram.html" <<'HTML'
<style>
  body { font: 15px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 18px 20px; color: #1f2328; background: #fff; }
  h2 { font-size: 19px; margin: 0 0 4px; }
  p { margin: 0 0 10px; color: #59636e; }
  svg { display: block; width: 100%; height: auto; }
  .box { fill: #ddf4ff; stroke: #0969da; stroke-width: 2; }
  .ok { fill: #dafbe1; stroke: #1a7f37; stroke-width: 2; }
  .bad { fill: #ffebe9; stroke: #cf222e; stroke-width: 2; }
  .t { font-size: 17px; font-weight: 600; fill: #1f2328; text-anchor: middle; }
  .s { font-size: 13px; fill: #59636e; text-anchor: middle; }
  .arrow { stroke: #59636e; stroke-width: 2.5; fill: none; marker-end: url(#h); }
  .retry { stroke: #cf222e; stroke-width: 2.5; fill: none; stroke-dasharray: 6 5; marker-end: url(#r); }
</style>
<h2>Checkout retry: where a rejected order stops</h2>
<p>A 4xx from Payments now ends the request instead of looping back.</p>
<svg viewBox="0 0 800 170" role="img" aria-label="Flow from Cart to Checkout API to Payments to Ledger, with a retry loop that stops on a 4xx">
  <defs>
    <marker id="h" markerWidth="10" markerHeight="10" refX="8" refY="5" orient="auto"><path d="M0 0L10 5L0 10z" fill="#59636e"/></marker>
    <marker id="r" markerWidth="10" markerHeight="10" refX="8" refY="5" orient="auto"><path d="M0 0L10 5L0 10z" fill="#cf222e"/></marker>
  </defs>
  <rect class="box" x="10" y="40" width="150" height="64" rx="10"/><text class="t" x="85" y="70">Cart</text><text class="s" x="85" y="90">web</text>
  <rect class="box" x="220" y="40" width="170" height="64" rx="10"/><text class="t" x="305" y="70">Checkout API</text><text class="s" x="305" y="90">retries 5xx</text>
  <rect class="box" x="450" y="40" width="150" height="64" rx="10"/><text class="t" x="525" y="70">Payments</text><text class="s" x="525" y="90">card network</text>
  <rect class="ok" x="660" y="40" width="130" height="64" rx="10"/><text class="t" x="725" y="70">Ledger</text><text class="s" x="725" y="90">order saved</text>
  <path class="arrow" d="M160 72H216"/><path class="arrow" d="M390 72H446"/><path class="arrow" d="M600 72H656"/>
  <path class="retry" d="M525 104V146H305V108"/>
  <rect class="bad" x="380" y="128" width="90" height="34" rx="8"/><text class="t" x="425" y="151" style="font-size:15px">4xx: stop</text>
</svg>
HTML

cat > "${work}/chart.html" <<'HTML'
<style>
  body { font: 15px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 18px 20px; color: #1f2328; background: #fff; }
  h2 { font-size: 19px; margin: 0 0 4px; }
  p { margin: 0 0 10px; color: #59636e; }
  svg { display: block; width: 100%; height: auto; }
  .grid { stroke: #d1d9e0; }
  .axis { fill: #59636e; font-size: 12px; }
  .before { fill: #c4cbd3; }
  .after { fill: #0969da; }
  .legend { font-size: 13px; fill: #1f2328; }
  .val { font-size: 13px; font-weight: 700; fill: #0969da; }
</style>
<h2>p95 latency after the invoice cache</h2>
<p>180ms down to 12ms on the busiest endpoint. Load test: 500 req/s for 10 minutes.</p>
<svg viewBox="0 0 800 250" role="img" aria-label="Bar chart of p95 latency before and after the cache for four endpoints">
  <g class="grid"><line x1="60" x2="790" y1="20" y2="20"/><line x1="60" x2="790" y1="70" y2="70"/><line x1="60" x2="790" y1="120" y2="120"/><line x1="60" x2="790" y1="170" y2="170"/><line x1="60" x2="790" y1="220" y2="220"/></g>
  <g class="axis" text-anchor="end"><text x="52" y="24">200ms</text><text x="52" y="74">150ms</text><text x="52" y="124">100ms</text><text x="52" y="174">50ms</text><text x="52" y="224">0</text></g>
  <rect class="before" x="100" y="40" width="60" height="180" rx="3"/><rect class="after" x="166" y="208" width="60" height="12" rx="3"/><text class="val" x="196" y="202" text-anchor="middle">12</text>
  <rect class="before" x="280" y="75" width="60" height="145" rx="3"/><rect class="after" x="346" y="211" width="60" height="9" rx="3"/><text class="val" x="376" y="205" text-anchor="middle">9</text>
  <rect class="before" x="460" y="100" width="60" height="120" rx="3"/><rect class="after" x="526" y="206" width="60" height="14" rx="3"/><text class="val" x="556" y="200" text-anchor="middle">14</text>
  <rect class="before" x="640" y="130" width="60" height="90" rx="3"/><rect class="after" x="706" y="210" width="60" height="10" rx="3"/><text class="val" x="736" y="204" text-anchor="middle">10</text>
  <g class="axis" text-anchor="middle"><text x="163" y="240">GET /invoices</text><text x="343" y="240">GET /invoices/:id</text><text x="523" y="240">GET /customers/:id</text><text x="703" y="240">POST /charges</text></g>
  <g class="legend"><rect class="before" x="610" y="2" width="12" height="12" rx="2"/><text x="628" y="12">before</text><rect class="after" x="690" y="2" width="12" height="12" rx="2"/><text x="708" y="12">after</text></g>
</svg>
HTML

cat > "${work}/shot.html" <<HTML
<style>
  body { font: 15px/1.5 -apple-system, system-ui, sans-serif; margin: 0; padding: 18px 20px; color: #1f2328; background: #fff; }
  h2 { font-size: 19px; margin: 0 0 8px; }
  img { display: block; width: 100%; border: 1px solid #d1d9e0; border-radius: 8px; }
</style>
<h2>Checkout page after the fix</h2>
<img style="width:60%" src="${work}/checkout.png" alt="Checkout page with the payment form and order summary">
HTML

cat > "${work}/plan.md" <<'MD'
## Plan: ship the docs navigation

1. Move Installing and Webhooks into Guides: **done**
2. Keep every old URL working: **done**
3. Add redirects for the four renamed pages: **next**
4. Publish and check the sitemap
MD

# post <session id> <cwd dir> <file> <format>
post() {
  (cd "${work}/cwd/$2" && CANVAS_SOCKET="${socket}" CLAUDE_CODE_SESSION_ID="$1" "${canvas}" post "$3" --format "$4" > /dev/null)
}


clear_stream() {
  for id in "${sessions[@]}"; do
    curl -s -o /dev/null --unix-socket "${socket}" -X DELETE "http://localhost/api/sessions/${id}" || true
  done
}

# Hide the "Canvas was updated" banner a dev build can show.
bash "${verify}" eval - <<'JS'
document.getElementById("update-banner-dismiss")?.click();
JS
sleep 1

if [ "${ADMIN_CLIP:-}" = "1" ]; then
  until [ -e "${ADMIN_CLIP_START:?ADMIN_CLIP=1 needs ADMIN_CLIP_START}" ]; do sleep 0.05; done
fi

# at <seconds> — sleep until that many seconds after the cycle's t0.
at() { python3 -c "import time;time.sleep(max(0,$1-(time.time()-${t0})))"; }

while true; do
  t0=$(python3 -c 'import time;print(time.time())')
  at 0.7; post demo-storefront storefront "${work}/diagram.html" html
  at 1.7; post demo-billing billing-api "${work}/chart.html" html
  at 2.7; post demo-web web "${work}/shot.html" html
  at 3.7; post demo-docs docs-site "${work}/plan.md" md
  at 4.9; clear_stream
  at "${PERIOD}"
done
