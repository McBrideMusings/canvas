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

  // Only Canvas.app's WKWebView injects window.__TAURI__ (withGlobalTauri in
  // tauri.conf.json); a plain browser tab never sees it, which is how the
  // button knows to stay hidden there.
  const pinToggleEl = document.getElementById("pin-toggle");
  const tauriCore = window.__TAURI__ && window.__TAURI__.core;

  function applyPinnedState(pinned) {
    pinToggleEl.setAttribute("aria-pressed", pinned ? "true" : "false");
    pinToggleEl.setAttribute(
      "aria-label",
      pinned ? "Unpin window" : "Pin window on top"
    );
  }

  if (tauriCore) {
    pinToggleEl.hidden = false;
    tauriCore
      .invoke("get_pinned")
      .then((pinned) => applyPinnedState(!!pinned))
      .catch(() => {});

    pinToggleEl.addEventListener("click", async () => {
      const nextPinned = pinToggleEl.getAttribute("aria-pressed") !== "true";
      try {
        await tauriCore.invoke("set_pinned", { pinned: nextPinned });
        applyPinnedState(nextPinned);
      } catch (e) {
        // Refused invoke (wrong origin, missing capability) — leave the
        // button's displayed state as it was.
      }
    });
  }

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
  // which button (by session id, or the sentinel below for "All", plus which
  // of the two buttons in the row) had focus beforehand so it can be
  // restored to its replacement afterward.
  const ALL_ROW_KEY = "__all__";

  function focusKeyFor(el) {
    return `${el.dataset.sessionId ?? ALL_ROW_KEY}::${el.dataset.role}`;
  }

  function renderSidebar() {
    const active = document.activeElement;
    const focusedKey =
      active && sidebarEl.contains(active) ? focusKeyFor(active) : null;

    // A rebuild is about to detach every row, including any button an armed
    // confirm is pointing at — clear it first so the state machine never
    // holds a reference to a node that is no longer in the document, then
    // re-arm the rebuilt button for the same session with the time it had
    // left, so a card arriving mid-confirm doesn't cancel the clear.
    let rearm = null;
    if (pendingConfirm && sidebarEl.contains(pendingConfirm.button)) {
      rearm = {
        sessionId: pendingConfirm.button.dataset.sessionId,
        opts: pendingConfirm.opts,
        ms: pendingConfirm.deadline - Date.now(),
      };
      clearPendingConfirm();
    }

    sidebarEl.innerHTML = "";

    sidebarEl.appendChild(
      buildSidebarRow(ALL_ROW_KEY, "All", null, selectedSessionId === null, focusedKey)
    );

    for (const s of sessions.values()) {
      sidebarEl.appendChild(
        buildSidebarRow(s.id, s.name, s, selectedSessionId === s.id, focusedKey)
      );
    }

    if (rearm && rearm.ms > 0) {
      const btn = sidebarEl.querySelector(
        `.sidebar-row-delete[data-session-id="${CSS.escape(rearm.sessionId)}"]`
      );
      if (btn) armConfirm(btn, rearm.opts, rearm.ms);
    }
  }

  // A row is a container holding the select button and (for a real session,
  // not "All") a trash button beside it — never nested inside the select
  // button, since the select button is itself a real <button> (needed for
  // keyboard reachability) and buttons cannot nest.
  function buildSidebarRow(sessionId, name, session, selected, focusedKey) {
    const wrap = document.createElement("div");
    wrap.className = "sidebar-row-wrap";

    const row = document.createElement("button");
    row.type = "button";
    row.className = "sidebar-row" + (selected ? " selected" : "");
    row.setAttribute("aria-pressed", selected ? "true" : "false");
    row.dataset.sessionId = sessionId;
    row.dataset.role = "select";

    if (session) {
      const nameEl = document.createElement("div");
      nameEl.className = "sidebar-row-name";
      nameEl.textContent = name;
      row.appendChild(nameEl);

      const meta = document.createElement("div");
      meta.className = "sidebar-row-meta";
      const status = document.createElement("span");
      status.className = "sidebar-row-status";
      status.textContent = session.endedAt ? "ended" : "active";
      const count = sessionCardCount(session.id);
      const countSpan = document.createElement("span");
      countSpan.className = "sidebar-row-count";
      countSpan.textContent = ` · ${count} card${count === 1 ? "" : "s"}`;
      meta.appendChild(status);
      meta.appendChild(countSpan);
      row.appendChild(meta);
    } else {
      row.textContent = name;
    }

    row.addEventListener("click", () => selectSession(session ? session.id : null));
    wrap.appendChild(row);
    if (focusedKey === focusKeyFor(row)) row.focus();

    if (session) {
      const delBtn = document.createElement("button");
      delBtn.type = "button";
      delBtn.className = "icon-btn sidebar-row-delete";
      delBtn.dataset.sessionId = sessionId;
      delBtn.dataset.role = "delete";
      delBtn.setAttribute("aria-label", "Clear session");
      delBtn.textContent = "\u{1F5D1}";
      delBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        armConfirm(delBtn, {
          idleLabel: "Clear session",
          idleText: "\u{1F5D1}",
          confirmLabel: "Confirm clear",
          confirmText: "✓",
          onConfirm: () => clearSession(session.id),
        });
      });
      wrap.appendChild(delBtn);
      if (focusedKey === focusKeyFor(delBtn)) delBtn.focus();
    }

    return wrap;
  }

  // One button may be armed to "confirm" at a time. A second click on the
  // same (still-armed) button runs the action; a click anywhere else, the
  // Escape key, or 4s passing reverts it back to idle instead.
  let pendingConfirm = null;

  function clearPendingConfirm() {
    if (!pendingConfirm) return;
    clearTimeout(pendingConfirm.timeoutId);
    pendingConfirm.revert();
    pendingConfirm = null;
  }

  function armConfirm(button, opts, ms = 4000) {
    const { idleLabel, idleText, confirmLabel, confirmText, onConfirm } = opts;
    if (pendingConfirm && pendingConfirm.button === button) {
      clearPendingConfirm();
      onConfirm();
      return;
    }
    clearPendingConfirm();
    button.textContent = confirmText;
    button.setAttribute("aria-label", confirmLabel);
    button.classList.add("confirming");
    const timeoutId = setTimeout(clearPendingConfirm, ms);
    pendingConfirm = {
      button,
      opts,
      deadline: Date.now() + ms,
      timeoutId,
      revert() {
        button.textContent = idleText;
        button.setAttribute("aria-label", idleLabel);
        button.classList.remove("confirming");
      },
    };
  }

  document.addEventListener("click", (e) => {
    if (pendingConfirm && !pendingConfirm.button.contains(e.target)) {
      clearPendingConfirm();
    }
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && pendingConfirm) {
      clearPendingConfirm();
    }
  });

  function deleteCard(id) {
    fetch(`/api/cards/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
  }

  function clearSession(id) {
    fetch(`/api/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
  }

  // The server is the source of truth for removal: these run only once the
  // card-removed / session-removed SSE event arrives, so every open viewer
  // (including the one that clicked Confirm) stays in sync the same way.
  function removeCard(id) {
    if (!cards.has(id)) return;
    cards.delete(id);
    const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(id)}"]`);
    if (el) el.remove();
    applyFilter();
    renderSidebar();
  }

  function removeSession(id) {
    if (!sessions.has(id)) return;
    sessions.delete(id);
    for (const [cardId, c] of Array.from(cards.entries())) {
      if (c.sessionId !== id) continue;
      cards.delete(cardId);
      const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(cardId)}"]`);
      if (el) el.remove();
    }
    if (selectedSessionId === id) selectedSessionId = null;
    renderSidebar();
    applyFilter();
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

  const SVG_NS = "http://www.w3.org/2000/svg";

  function svgEl(tag, attrs) {
    const el = document.createElementNS(SVG_NS, tag);
    for (const key in attrs) {
      el.setAttribute(key, attrs[key]);
    }
    return el;
  }

  // SF Symbols-style glyphs, built with createElementNS (never innerHTML).
  // Shared by any icon button; canvas-8ie.2.13 reuses this for the rest of
  // the Mac restyle's icons.
  function buildIcon(name) {
    const svg = svgEl("svg", {
      viewBox: "0 0 24 24",
      width: "16",
      height: "16",
      fill: "none",
      stroke: "currentColor",
      "stroke-width": "1.5",
      "stroke-linecap": "round",
      "stroke-linejoin": "round",
      "aria-hidden": "true",
      focusable: "false",
    });
    if (name === "open") {
      svg.appendChild(
        svgEl("rect", { x: "3", y: "3", width: "18", height: "18", rx: "4" })
      );
      svg.appendChild(svgEl("path", { d: "M9 15L15 9" }));
      svg.appendChild(svgEl("path", { d: "M11 9h4v4" }));
    } else if (name === "copy") {
      svg.appendChild(
        svgEl("rect", { x: "8", y: "2", width: "13", height: "15", rx: "2" })
      );
      svg.appendChild(
        svgEl("rect", { x: "3", y: "7", width: "13", height: "15", rx: "2" })
      );
    } else if (name === "check") {
      svg.appendChild(svgEl("path", { d: "M5 13l4 4L19 7" }));
    }
    return svg;
  }

  function createIconButton(iconName, ariaLabel, onClick) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "path-icon-btn";
    btn.setAttribute("aria-label", ariaLabel);
    btn.appendChild(buildIcon(iconName));
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      onClick(btn);
    });
    return btn;
  }

  function setButtonIcon(button, iconName) {
    while (button.firstChild) {
      button.removeChild(button.firstChild);
    }
    button.appendChild(buildIcon(iconName));
  }

  function showCopied(button) {
    clearTimeout(button._copyTimer);
    setButtonIcon(button, "check");
    button.classList.add("copied");
    button.setAttribute("aria-label", "Copied");
    button._copyTimer = setTimeout(() => {
      setButtonIcon(button, "copy");
      button.classList.remove("copied");
      button.setAttribute("aria-label", "Copy path");
    }, 1500);
  }

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
        showCopied(button);
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
    const headerInfo = document.createElement("div");
    headerInfo.className = "card-header-info";
    const nameSpan = document.createElement("span");
    nameSpan.className = "session-name";
    nameSpan.textContent = sessionName(card.sessionId);
    const sep = document.createTextNode(" · ");
    const timeSpan = document.createElement("span");
    timeSpan.className = "time";
    timeSpan.textContent = relativeTime(card.at);
    headerInfo.appendChild(nameSpan);
    headerInfo.appendChild(sep);
    headerInfo.appendChild(timeSpan);
    header.appendChild(headerInfo);

    const delBtn = document.createElement("button");
    delBtn.type = "button";
    delBtn.className = "icon-btn card-delete-btn";
    delBtn.setAttribute("aria-label", "Delete card");
    delBtn.textContent = "×";
    delBtn.addEventListener("click", (e) => {
      e.stopPropagation();
      armConfirm(delBtn, {
        idleLabel: "Delete card",
        idleText: "×",
        confirmLabel: "Confirm delete",
        confirmText: "✓",
        onConfirm: () => deleteCard(card.id),
      });
    });
    header.appendChild(delBtn);
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
      row.appendChild(text);

      const actions = document.createElement("div");
      actions.className = "path-row-actions";
      actions.appendChild(
        createIconButton("open", "Open file", () => openTarget(path))
      );
      actions.appendChild(
        createIconButton("copy", "Copy path", (btn) => copyToClipboard(path, btn))
      );
      row.appendChild(actions);

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
    if (existing) {
      // The replacement is about to detach this card's delete button too —
      // clear an armed confirm pointing at it first, the same way
      // renderSidebar does for a sidebar rebuild, so pendingConfirm never
      // holds a reference to a node no longer in the document.
      if (pendingConfirm && existing.contains(pendingConfirm.button)) {
        clearPendingConfirm();
      }
      existing.remove();
    }

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

    source.addEventListener("card-removed", (event) => {
      const { id } = JSON.parse(event.data);
      removeCard(id);
    });

    source.addEventListener("session-removed", (event) => {
      const { id } = JSON.parse(event.data);
      removeSession(id);
    });
  }

  loadState().then(connectEvents);
})();
