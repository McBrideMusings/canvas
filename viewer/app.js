(() => {
  "use strict";

  const sessions = new Map(); // id -> session
  const cards = new Map(); // id -> card
  let selectedSessionId = null; // null = "All"

  const streamEl = document.getElementById("stream");
  const cardsEl = document.getElementById("cards");
  const emptyStateEl = document.getElementById("empty-state");
  const sidebarEl = document.getElementById("sidebar");
  const bannerEl = document.getElementById("disconnected-banner");
  const overlayEl = document.getElementById("image-overlay");
  const overlayImgEl = document.getElementById("image-overlay-img");
  const layoutEl = document.querySelector(".layout");
  const sidebarToggleEl = document.getElementById("sidebar-toggle");

  const SIDEBAR_HIDDEN_KEY = "canvas.sidebarHidden";

  // localStorage can throw (private browsing, blocked site data) — the
  // toggle still has to work within the page's own lifetime even then.
  function loadSidebarHidden() {
    try {
      return localStorage.getItem(SIDEBAR_HIDDEN_KEY) === "1";
    } catch (e) {
      return false;
    }
  }

  function saveSidebarHidden(hidden) {
    try {
      localStorage.setItem(SIDEBAR_HIDDEN_KEY, hidden ? "1" : "0");
    } catch (e) {}
  }

  let sidebarHidden = loadSidebarHidden();

  function applySidebarHidden() {
    layoutEl.classList.toggle("sidebar-hidden", sidebarHidden);
    sidebarToggleEl.setAttribute("aria-expanded", sidebarHidden ? "false" : "true");
    sidebarToggleEl.setAttribute(
      "aria-label",
      sidebarHidden ? "Show sessions" : "Hide sessions"
    );
  }

  applySidebarHidden();

  sidebarToggleEl.addEventListener("click", () => {
    sidebarHidden = !sidebarHidden;
    applySidebarHidden();
    saveSidebarHidden(sidebarHidden);
  });

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

  function sessionCardCount(sessionId) {
    let n = 0;
    for (const c of cards.values()) {
      if (c.sessionId === sessionId) n++;
    }
    return n;
  }

  function selectSession(id) {
    selectedSessionId = id;
    renderSidebar();
    applyFilter();
  }

  // Rebuilding every row from scratch on each render drops keyboard focus,
  // since the previously focused button is removed from the document. Note
  // which row (by session id, or the sentinel below for "All") had focus
  // beforehand so it can be restored to its replacement afterward.
  const ALL_ROW_KEY = "__all__";

  function renderSidebar() {
    const active = document.activeElement;
    const focusedKey =
      active && sidebarEl.contains(active)
        ? active.dataset.sessionId ?? ALL_ROW_KEY
        : null;

    sidebarEl.innerHTML = "";

    const all = document.createElement("button");
    all.type = "button";
    all.className =
      "sidebar-row" + (selectedSessionId === null ? " selected" : "");
    all.setAttribute("aria-pressed", selectedSessionId === null ? "true" : "false");
    all.dataset.sessionId = ALL_ROW_KEY;
    all.textContent = "All";
    all.addEventListener("click", () => selectSession(null));
    sidebarEl.appendChild(all);
    if (focusedKey === ALL_ROW_KEY) all.focus();

    for (const s of sessions.values()) {
      const row = document.createElement("button");
      row.type = "button";
      row.className =
        "sidebar-row" + (selectedSessionId === s.id ? " selected" : "");
      row.setAttribute("aria-pressed", selectedSessionId === s.id ? "true" : "false");
      row.dataset.sessionId = s.id;

      const name = document.createElement("div");
      name.className = "sidebar-row-name";
      name.textContent = s.name;
      row.appendChild(name);

      const meta = document.createElement("div");
      meta.className = "sidebar-row-meta";
      const status = document.createElement("span");
      status.className = "sidebar-row-status";
      status.textContent = s.endedAt ? "ended" : "active";
      const count = sessionCardCount(s.id);
      const countSpan = document.createElement("span");
      countSpan.className = "sidebar-row-count";
      countSpan.textContent = ` · ${count} card${count === 1 ? "" : "s"}`;
      meta.appendChild(status);
      meta.appendChild(countSpan);
      row.appendChild(meta);

      row.addEventListener("click", () => selectSession(s.id));
      sidebarEl.appendChild(row);
      if (focusedKey === s.id) row.focus();
    }
  }

  function cardMatchesFilter(sessionId) {
    return selectedSessionId === null || sessionId === selectedSessionId;
  }

  // Hide/show existing card elements per the current filter without
  // touching any other card's DOM node (and any live iframe inside it).
  function applyFilter() {
    let visible = 0;
    for (const el of cardsEl.children) {
      const matches = cardMatchesFilter(el.dataset.sessionId);
      el.hidden = !matches;
      if (matches) visible++;
    }
    updateEmptyState(visible);
  }

  // The empty-state message differs depending on whether there are no
  // posts at all, or just none matching the current sidebar filter.
  function updateEmptyState(visible) {
    emptyStateEl.hidden = visible > 0;
    emptyStateEl.textContent =
      cards.size === 0
        ? "No posts yet. Canvas shows links, files and images from your Claude sessions as they work."
        : "No cards yet for this session.";
  }

  function buildIframeDoc(html) {
    const csp =
      "default-src 'none'; " +
      "script-src https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com 'unsafe-inline'; " +
      "style-src 'unsafe-inline' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com https://fonts.googleapis.com; " +
      "font-src https://fonts.gstatic.com data:; " +
      "img-src * data:;";
    // Measure the content wrapper, not documentElement: documentElement's
    // scrollHeight is clamped to at least the iframe's own current viewport
    // height, so once the parent sets a height it can never report a
    // smaller one again (the "ratchets up, never down" bug). The wrapper
    // has no imposed height, so its own box always equals its content's
    // extent. `overflow:hidden` on it opens a new block formatting context
    // so children's margins don't collapse out through it and go
    // unmeasured.
    const resizeScript = `
      <script>
        var canvasRoot = document.getElementById('__canvas_root');
        function canvasSendHeight() {
          var h = Math.ceil(canvasRoot.getBoundingClientRect().height);
          parent.postMessage({ type: 'canvas-resize', height: h }, '*');
        }
        window.addEventListener('load', canvasSendHeight);
        window.addEventListener('resize', canvasSendHeight);
        try {
          new ResizeObserver(canvasSendHeight).observe(canvasRoot);
        } catch (e) {}
        setInterval(canvasSendHeight, 400);
      </script>
    `;
    return (
      `<!doctype html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
      `<style>html,body{margin:0;overflow:hidden;}` +
      `body{font-family:-apple-system,sans-serif;}` +
      `#__canvas_root{overflow:hidden;}</style>` +
      `</head><body><div id="__canvas_root">${html}</div>${resizeScript}</body></html>`
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
    cardsEl.innerHTML = "";
    for (const card of sorted) {
      cardsEl.appendChild(renderCard(card));
    }
    applyFilter();
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
    el.hidden = !cardMatchesFilter(card.sessionId);
    const siblings = Array.from(cardsEl.children);
    const insertBefore = siblings.find(
      (child) => new Date(child.dataset.at) < new Date(card.at)
    );
    if (insertBefore) {
      cardsEl.insertBefore(el, insertBefore);
    } else {
      cardsEl.appendChild(el);
    }

    applyFilter();

    // A new card changes its session's count — refresh the sidebar without
    // touching the stream, so a card for a non-selected session updates its
    // row's count and leaves the visible stream alone.
    renderSidebar();
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
