(() => {
  "use strict";

  const sessions = new Map(); // id -> session
  const cards = new Map(); // id -> card

  const streamEl = document.getElementById("stream");
  const cardsEl = document.getElementById("cards");
  const emptyStateEl = document.getElementById("empty-state");
  const sidebarEl = document.getElementById("sidebar");
  const bannerEl = document.getElementById("disconnected-banner");
  const overlayEl = document.getElementById("image-overlay");
  const overlayImgEl = document.getElementById("image-overlay-img");

  function relativeTime(iso) {
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return "";
    const seconds = Math.max(0, Math.floor((Date.now() - then) / 1000));
    if (seconds < 5) return "just now";
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  }

  function sessionName(sessionId) {
    const s = sessions.get(sessionId);
    return s ? s.name : sessionId;
  }

  function renderSidebar() {
    sidebarEl.innerHTML = "";
    const all = document.createElement("div");
    all.className = "sidebar-row selected";
    all.textContent = "All";
    sidebarEl.appendChild(all);

    for (const s of sessions.values()) {
      const row = document.createElement("div");
      row.className = "sidebar-row";
      row.textContent = s.name;
      sidebarEl.appendChild(row);
    }
  }

  function buildIframeDoc(html) {
    const csp =
      "default-src 'none'; " +
      "script-src https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com 'unsafe-inline'; " +
      "style-src 'unsafe-inline' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com https://fonts.googleapis.com; " +
      "font-src https://fonts.gstatic.com data:; " +
      "img-src * data:;";
    const resizeScript = `
      <script>
        function canvasSendHeight() {
          var h = document.documentElement.scrollHeight;
          parent.postMessage({ type: 'canvas-resize', height: h }, '*');
        }
        window.addEventListener('load', canvasSendHeight);
        window.addEventListener('resize', canvasSendHeight);
        try {
          new ResizeObserver(canvasSendHeight).observe(document.documentElement);
        } catch (e) {}
        setInterval(canvasSendHeight, 400);
      </script>
    `;
    return (
      `<!doctype html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
      `<style>body{margin:0;font-family:-apple-system,sans-serif;}</style>` +
      `</head><body>${html}${resizeScript}</body></html>`
    );
  }

  window.addEventListener("message", (event) => {
    const data = event.data;
    if (!data || data.type !== "canvas-resize") return;
    const frames = cardsEl.querySelectorAll("iframe");
    for (const frame of frames) {
      if (frame.contentWindow === event.source) {
        frame.style.height = `${Math.max(20, data.height)}px`;
        break;
      }
    }
  });

  function copyToClipboard(text, button) {
    navigator.clipboard
      .writeText(text)
      .catch(() => {
        const ta = document.createElement("textarea");
        ta.value = text;
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      })
      .finally(() => {
        const original = button.textContent;
        button.textContent = "Copied";
        setTimeout(() => {
          button.textContent = original;
        }, 1500);
      });
  }

  function openTarget(path) {
    fetch("/api/open", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path }),
    }).catch(() => {});
  }

  function renderCard(card) {
    const el = document.createElement("div");
    el.className = "card";
    el.dataset.cardId = card.id;
    el.dataset.sessionId = card.sessionId;
    el.dataset.at = card.at;

    const header = document.createElement("div");
    header.className = "card-header";
    const nameSpan = document.createElement("span");
    nameSpan.className = "session-name";
    nameSpan.textContent = sessionName(card.sessionId);
    const sep = document.createTextNode(" · ");
    const timeSpan = document.createElement("span");
    timeSpan.className = "time";
    timeSpan.textContent = relativeTime(card.at);
    header.appendChild(nameSpan);
    header.appendChild(sep);
    header.appendChild(timeSpan);
    el.appendChild(header);

    const body = document.createElement("div");
    body.className = "card-body";

    for (const html of card.html) {
      const iframe = document.createElement("iframe");
      iframe.setAttribute("sandbox", "allow-scripts");
      iframe.srcdoc = buildIframeDoc(html);
      body.appendChild(iframe);
    }

    for (const imagePath of card.images) {
      const img = document.createElement("img");
      img.className = "card-image";
      img.src = `/api/file?path=${encodeURIComponent(imagePath)}`;
      img.alt = "";
      img.addEventListener("click", () => {
        overlayImgEl.src = img.src;
        overlayEl.hidden = false;
      });
      body.appendChild(img);
    }

    for (const link of card.links) {
      const row = document.createElement("div");
      row.className = "row link-row";
      const text = document.createElement("span");
      text.className = "row-text";
      text.textContent = link;
      row.appendChild(text);
      row.addEventListener("click", () => openTarget(link));
      body.appendChild(row);
    }

    for (const path of card.paths) {
      const row = document.createElement("div");
      row.className = "row path-row";
      const text = document.createElement("span");
      text.className = "row-text";
      text.textContent = path;
      const copyBtn = document.createElement("button");
      copyBtn.className = "copy-btn";
      copyBtn.textContent = "Copy";
      copyBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        copyToClipboard(path, copyBtn);
      });
      row.appendChild(text);
      row.appendChild(copyBtn);
      row.addEventListener("click", () => openTarget(path));
      body.appendChild(row);
    }

    el.appendChild(body);
    return el;
  }

  function sortedCards() {
    return Array.from(cards.values()).sort(
      (a, b) => new Date(b.at) - new Date(a.at)
    );
  }

  // Full rebuild — used for the initial load and for resyncing after an SSE
  // reconnect, where cards may have changed without the browser seeing it.
  function bootstrapRender() {
    renderSidebar();
    const sorted = sortedCards();
    emptyStateEl.hidden = sorted.length > 0;
    cardsEl.innerHTML = "";
    for (const card of sorted) {
      cardsEl.appendChild(renderCard(card));
    }
  }

  // Insert or replace only the one card that changed, leaving every other
  // card's DOM node (and any live iframe inside it) untouched, so a card
  // updated elsewhere never reloads this card's sandboxed HTML posts.
  function upsertCard(card) {
    cards.set(card.id, card);

    const existing = cardsEl.querySelector(
      `[data-card-id="${CSS.escape(card.id)}"]`
    );
    if (existing) existing.remove();

    const el = renderCard(card);
    const siblings = Array.from(cardsEl.children);
    const insertBefore = siblings.find(
      (child) => new Date(child.dataset.at) < new Date(card.at)
    );
    if (insertBefore) {
      cardsEl.insertBefore(el, insertBefore);
    } else {
      cardsEl.appendChild(el);
    }

    emptyStateEl.hidden = cards.size > 0;
  }

  // A session's name can change (e.g. re-registration); patch just the
  // header text of that session's existing cards instead of rebuilding them.
  function patchSessionNames(sessionId) {
    const name = sessionName(sessionId);
    const selector = `.card[data-session-id="${CSS.escape(sessionId)}"] .session-name`;
    for (const el of cardsEl.querySelectorAll(selector)) {
      el.textContent = name;
    }
  }

  overlayEl.addEventListener("click", () => {
    overlayEl.hidden = true;
    overlayImgEl.src = "";
  });
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !overlayEl.hidden) {
      overlayEl.hidden = true;
      overlayImgEl.src = "";
    }
  });

  async function loadState() {
    const res = await fetch("/api/state");
    const data = await res.json();
    sessions.clear();
    for (const s of data.sessions) sessions.set(s.id, s);
    cards.clear();
    for (const c of data.cards) cards.set(c.id, c);
    bootstrapRender();
  }

  function connectEvents() {
    const source = new EventSource("/api/events");
    let hasConnectedOnce = false;

    source.addEventListener("open", () => {
      bannerEl.hidden = true;
      // A reconnect (not the first connection) means events published while
      // this browser was disconnected went to a broadcast channel with no
      // subscriber and are gone for good — re-fetch the full snapshot so the
      // view catches back up instead of silently missing them forever.
      if (hasConnectedOnce) {
        loadState();
      }
      hasConnectedOnce = true;
    });

    source.addEventListener("error", () => {
      bannerEl.hidden = false;
    });

    source.addEventListener("session-upserted", (event) => {
      const session = JSON.parse(event.data);
      sessions.set(session.id, session);
      renderSidebar();
      patchSessionNames(session.id);
    });

    source.addEventListener("card-upserted", (event) => {
      const card = JSON.parse(event.data);
      upsertCard(card);
    });
  }

  loadState().then(connectEvents);
})();
