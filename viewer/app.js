(() => {
  "use strict";

  const sessions = new Map(); // id -> session
  const cards = new Map(); // id -> card
  // Three states: null = every session visible (nothing selected). A Set,
  // however small, means "only these" — chips and the drawer's per-row eye
  // toggle both read and write it, so picking one in either place narrows
  // the other. Selecting a session for the first time (from null) isolates
  // to just that one; after that, each control just toggles its own id in
  // or out of the set. Only resetVisibility() goes back to null.
  let visibleSessionIds = null;

  const streamEl = document.getElementById("stream");
  const cardsEl = document.getElementById("cards");
  const mainColEl = document.querySelector(".main-col");
  const bodySplitEl = document.querySelector(".body-split");
  const emptyStateEl = document.getElementById("empty-state");
  const chipsEl = document.getElementById("chips");
  // A trackpad's horizontal swipe scrolls the chip row natively; a plain
  // mouse wheel only ever sends a vertical delta, which the row has no
  // vertical overflow to consume. Redirect it to horizontal scroll, but only
  // when vertical actually dominates — a trackpad's diagonal swipe already
  // has its own deltaX and should scroll natively, untouched.
  chipsEl.addEventListener(
    "wheel",
    (e) => {
      if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
        chipsEl.scrollLeft += e.deltaY;
        e.preventDefault();
      }
    },
    { passive: false }
  );
  const bannerEl = document.getElementById("disconnected-banner");
  const overlayEl = document.getElementById("image-overlay");
  const overlayImgEl = document.getElementById("image-overlay-img");
  const overlayPrevEl = document.getElementById("image-overlay-prev");
  const overlayNextEl = document.getElementById("image-overlay-next");
  const overlayCountEl = document.getElementById("image-overlay-count");
  const overlayCloseEl = document.getElementById("image-overlay-close");
  const overlayZoomEl = document.getElementById("image-overlay-zoom");
  const titlebarEl = document.getElementById("titlebar");
  const toastsEl = document.getElementById("toasts");

  const SVG_NS = "http://www.w3.org/2000/svg";

  function svgEl(tag, attrs) {
    const el = document.createElementNS(SVG_NS, tag);
    for (const key in attrs) {
      el.setAttribute(key, attrs[key]);
    }
    return el;
  }

  // SF Symbols-style glyphs, built with createElementNS (never innerHTML).
  // Shared by every icon button. An unknown name throws rather than
  // silently rendering an empty <svg>, so a typo'd icon name fails loudly
  // at the call site instead of shipping a blank button.
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
    switch (name) {
      case "copy":
        svg.appendChild(
          svgEl("rect", { x: "8", y: "2", width: "13", height: "15", rx: "2" })
        );
        svg.appendChild(
          svgEl("rect", { x: "3", y: "7", width: "13", height: "15", rx: "2" })
        );
        break;
      case "link":
        svg.appendChild(svgEl("path", { d: "M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" }));
        svg.appendChild(svgEl("path", { d: "M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" }));
        break;
      case "check":
        svg.appendChild(svgEl("path", { d: "M5 13l4 4L19 7" }));
        break;
      case "more":
        for (const cx of ["5.5", "12", "18.5"]) {
          svg.appendChild(
            svgEl("circle", { cx, cy: "12", r: "1.2", fill: "currentColor", stroke: "none" })
          );
        }
        break;
      case "filter":
        svg.appendChild(svgEl("path", { d: "M4 5h16l-6 7.5V19l-4 2v-8.5z" }));
        break;
      case "trash":
        svg.appendChild(svgEl("path", { d: "M4 7h16" }));
        svg.appendChild(
          svgEl("path", {
            d: "M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7",
          })
        );
        svg.appendChild(
          svgEl("path", { d: "M6 7l1 13a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-13" })
        );
        svg.appendChild(svgEl("path", { d: "M10 11v6" }));
        svg.appendChild(svgEl("path", { d: "M14 11v6" }));
        break;
      case "x":
        svg.appendChild(svgEl("path", { d: "M7 7l10 10M17 7L7 17" }));
        break;
      case "search":
        svg.appendChild(svgEl("circle", { cx: "10.5", cy: "10.5", r: "6.5" }));
        svg.appendChild(svgEl("path", { d: "M15.5 15.5L21 21" }));
        break;
      // The agent tile's mark: an eight-ray burst, drawn heavier than the
      // other glyphs because it sits small and white on a coloured tile.
      case "claude":
        svg.setAttribute("stroke-width", "2.7");
        svg.appendChild(
          svgEl("path", {
            d: "M12 3v18M3 12h18M5.7 5.7l12.6 12.6M18.3 5.7L5.7 18.3",
          })
        );
        break;
      // A thumbtack drawn upright — round head, flat collar, tapering body
      // to a point — then tilted 45° like SF Symbols' "pin". The group
      // rotates as a whole so the head and body stay one rigid shape.
      // An upright pushpin: flat cap, body flaring to a wide collar, needle.
      case "pin":
      case "pin-fill": {
        const fill = name === "pin-fill" ? "currentColor" : "none";
        svg.appendChild(svgEl("path", { d: "M8 3h8M9.5 3l-1 8.5M14.5 3l1 8.5", fill: "none" }));
        svg.appendChild(svgEl("path", { d: "M9.5 3h5l1 8.5h-7z", fill, stroke: "none" }));
        svg.appendChild(svgEl("rect", { x: "6", y: "11.5", width: "12", height: "2.5", rx: "1.25", fill }));
        svg.appendChild(svgEl("path", { d: "M12 14v7", fill: "none" }));
        break;
      }
      // The standard rounded-tooth settings cog (Feather Icons' "settings"
      // glyph, MIT-licensed) — the two hand-rolled attempts before this one
      // still read as a sun at icon size, so this uses a known-good shape
      // instead of another guess.
      case "gear":
        svg.appendChild(svgEl("circle", { cx: "12", cy: "12", r: "3" }));
        svg.appendChild(
          svgEl("path", {
            d: "M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 11-2.83 2.83l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 11-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 11-2.83-2.83l.06-.06a1.65 1.65 0 00.33-1.82 1.65 1.65 0 00-1.51-1H3a2 2 0 110-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 112.83-2.83l.06.06a1.65 1.65 0 001.82.33H9a1.65 1.65 0 001-1.51V3a2 2 0 114 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 112.83 2.83l-.06.06a1.65 1.65 0 00-.33 1.82V9a1.65 1.65 0 001.51 1H21a2 2 0 110 4h-.09a1.65 1.65 0 00-1.51 1z",
          })
        );
        break;
      // The icon shows the mode a click switches to.
      case "moon":
        svg.appendChild(svgEl("path", { d: "M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z" }));
        break;
      case "sun":
        svg.appendChild(svgEl("circle", { cx: "12", cy: "12", r: "4" }));
        svg.appendChild(
          svgEl("path", {
            d: "M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4",
          })
        );
        break;
      // A window with its sidebar on the right.
      case "sidebar-right":
        svg.appendChild(
          svgEl("rect", { x: "3", y: "4.5", width: "18", height: "15", rx: "2.5" })
        );
        svg.appendChild(svgEl("path", { d: "M15 4.5v15" }));
        break;
      case "archive":
      case "unarchive":
        svg.appendChild(svgEl("path", { d: "M3 4.5h18V8H3z" }));
        svg.appendChild(svgEl("path", { d: "M5 8v10.5A1.5 1.5 0 0 0 6.5 20h11a1.5 1.5 0 0 0 1.5-1.5V8" }));
        svg.appendChild(
          svgEl("path", {
            d: name === "archive" ? "M10 12h4" : "M12 17v-5.5M9.5 14l2.5-2.5 2.5 2.5",
          })
        );
        break;
      // An open eye — a diagonal strike marks the -off variant, shown when
      // a custom selection excludes this session.
      case "eye":
      case "eye-off":
        svg.appendChild(
          svgEl("path", {
            d: "M2 12c2.5-5 7-7.5 10-7.5S19.5 7 22 12c-2.5 5-7 7.5-10 7.5S4.5 17 2 12z",
          })
        );
        svg.appendChild(svgEl("circle", { cx: "12", cy: "12", r: "3" }));
        if (name === "eye-off") svg.appendChild(svgEl("path", { d: "M3 3l18 18" }));
        break;
      case "arrow-up":
        svg.appendChild(svgEl("path", { d: "M12 19V5M5.5 11.5L12 5l6.5 6.5" }));
        break;
      case "chevron-left":
        svg.appendChild(svgEl("path", { d: "M15 5l-7 7 7 7" }));
        break;
      case "chevron-right":
        svg.appendChild(svgEl("path", { d: "M9 5l7 7-7 7" }));
        break;
      case "x":
        svg.appendChild(svgEl("path", { d: "M6 6l12 12M18 6L6 18" }));
        break;
      default:
        throw new Error(`buildIcon: unknown icon name "${name}"`);
    }
    return svg;
  }

  // `text`, when given, follows the icon as the button's visible label (a
  // menu item); icon-only buttons leave it out and rely on aria-label.
  function setButtonIcon(button, iconName, text) {
    while (button.firstChild) {
      button.removeChild(button.firstChild);
    }
    if (iconName) button.appendChild(buildIcon(iconName));
    if (text !== undefined) {
      const label = document.createElement("span");
      label.className = "menu-item-label";
      label.textContent = text;
      button.appendChild(label);
    }
  }

  // window.__TAURI__ is only ever injected into Canvas.app's own WKWebView
  // (withGlobalTauri in tauri.conf.json) — a plain browser tab never sees
  // it, which is how the bar knows whether to leave room for the traffic
  // lights it doesn't draw itself.
  if (window.__TAURI__) {
    titlebarEl.classList.add("has-traffic-lights");
  }

  // Viewer preferences, one JSON object in one key. A missing or malformed
  // field takes its default. localStorage can throw (private browsing,
  // blocked site data); that only costs persistence: prefs keeps working in
  // memory for the page's life.
  const PREFS_KEY = "canvas.viewer";

  function loadPrefs() {
    let stored = {};
    try {
      const parsed = JSON.parse(localStorage.getItem(PREFS_KEY) || "{}");
      if (parsed && typeof parsed === "object") stored = parsed;
    } catch (e) {}
    const ids = (v) =>
      Array.isArray(v) ? v.filter((id) => typeof id === "string") : [];
    return {
      hideEnded: typeof stored.hideEnded === "boolean" ? stored.hideEnded : true,
      animate: typeof stored.animate === "boolean" ? stored.animate : true,
      colorBy: stored.colorBy === "session" ? "session" : "repo",
      fullWidth: typeof stored.fullWidth === "boolean" ? stored.fullWidth : false,
      shown: ids(stored.shown),
      hidden: ids(stored.hidden),
    };
  }

  const prefs = loadPrefs();

  function applyFullWidth() {
    document.documentElement.classList.toggle("full-width", prefs.fullWidth);
  }
  applyFullWidth();

  // What this window last wrote or read, so a stored value that differs is
  // known to come from the Settings window.
  let lastStoredPrefs = null;

  function readStoredPrefs() {
    try {
      return localStorage.getItem(PREFS_KEY);
    } catch (e) {
      return null;
    }
  }

  function savePrefs() {
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    } catch (e) {}
    lastStoredPrefs = readStoredPrefs();
  }

  // The Settings window writes the same key from its own document. The
  // `storage` event announces it; window focus covers a webview that doesn't
  // deliver the event across windows.
  function syncPrefsFromStorage() {
    const stored = readStoredPrefs();
    if (stored === lastStoredPrefs) return;
    lastStoredPrefs = stored;
    const colorBy = prefs.colorBy;
    Object.assign(prefs, loadPrefs());
    if (prefs.colorBy !== colorBy) recolourSessions();
    applyFullWidth();
    refreshVisibility();
  }

  window.addEventListener("storage", (e) => {
    if (e.key === PREFS_KEY || e.key === null) syncPrefsFromStorage();
  });
  window.addEventListener("focus", syncPrefsFromStorage);
  lastStoredPrefs = readStoredPrefs();

  // Arrival. WKWebView has no CSS scroll anchoring, so a reader scrolled
  // down more than SCROLLED_PX keeps their place by hand: the height of a
  // card inserted above them, and every later height change of a card above
  // them, is added to the stream's scrollTop. `overflow-anchor: none` in the
  // stylesheet stops browsers that do anchor from correcting a second time.
  const SCROLLED_PX = 40;
  const TO_TOP_PX = 500;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const unseen = { cardIds: new Set() };
  // True from a click on the pill or back-to-top button until the stream
  // reaches the top, so place-holding does not fight the smooth scroll.
  let returningToTop = false;
  const newPillEl = document.getElementById("new-pill");
  const toTopEl = document.getElementById("to-top");

  function animating() {
    return prefs.animate && !reducedMotion.matches;
  }

  // The one rule for an archived session. The stream filter, the chip row,
  // the Sessions drawer and its toggle's badge all ask it.
  function isHidden(session) {
    if (prefs.hidden.includes(session.id)) return true;
    return !!session.endedAt && prefs.hideEnded && !prefs.shown.includes(session.id);
  }

  function archivedSessions() {
    return Array.from(sessions.values()).filter(isHidden);
  }

  // Ids of sessions canvasd no longer knows would otherwise sit in storage
  // for good.
  function prunePrefs() {
    prefs.shown = prefs.shown.filter((id) => sessions.has(id));
    prefs.hidden = prefs.hidden.filter((id) => sessions.has(id));
    savePrefs();
  }

  // Session colours come from this fixed palette in the order their keys
  // first appear — the repo (or cwd, with no repo) by default, the session
  // id when Colour sessions by is Session. Never hashed, so two keys share a
  // colour only once more than ten are in play.
  const PALETTE = [
    "#c2410c", "#0f766e", "#7c3aed", "#be185d", "#1d4ed8",
    "#4d7c0f", "#b45309", "#0e7490", "#9f1239", "#4338ca",
  ];
  let colours = new Map(); // key -> palette entry

  function sessionColour(session) {
    const key =
      prefs.colorBy === "session" ? session.id : session.repo || session.cwd;
    if (!colours.has(key)) colours.set(key, PALETTE[colours.size % PALETTE.length]);
    return colours.get(key);
  }

  function paintCardColours() {
    for (const el of [...cardsEl.children, ...document.querySelectorAll(".widget")]) {
      const s = sessions.get(el.dataset.sessionId);
      if (s) el.style.setProperty("--session-colour", sessionColour(s));
    }
  }

  // Switching the key hands colours out again from the first palette entry,
  // in the order sessions first appeared. The chips and drawer repaint in
  // the refreshVisibility that follows.
  function recolourSessions() {
    colours = new Map();
    for (const s of sessions.values()) sessionColour(s);
    paintCardColours();
  }

  // An archived session can't stay in the visible set: its cards are
  // already filtered out behind isHidden, and its drawer row is gone.
  function refreshVisibility() {
    if (visibleSessionIds) {
      for (const id of visibleSessionIds) {
        const s = sessions.get(id);
        if (!s || isHidden(s)) visibleSessionIds.delete(id);
      }
    }
    renderChips();
    applyFilter();
    renderDrawer();
  }

  function setPrefsAndRefresh(change) {
    // Take in what the Settings window wrote first, so saving doesn't drop it.
    syncPrefsFromStorage();
    change();
    savePrefs();
    refreshVisibility();
  }

  // Archiving hides the session and permanently deletes its posts — "hide
  // it but leave an empty husk around" wasn't a distinction anyone wanted.
  // Undo below only restores visibility; the posts are already gone by the
  // time it could run.
  function archiveSession(id) {
    const name = sessionName(id);
    clearSessionPosts(id);
    setPrefsAndRefresh(() => {
      if (!prefs.hidden.includes(id)) prefs.hidden.push(id);
    });
    toast(`Archived ${name} and cleared its posts`, "Undo", () =>
      setPrefsAndRefresh(() => {
        prefs.hidden = prefs.hidden.filter((h) => h !== id);
      })
    );
  }

  function showSession(id) {
    setPrefsAndRefresh(() => {
      prefs.hidden = prefs.hidden.filter((h) => h !== id);
      if (!prefs.shown.includes(id)) prefs.shown.push(id);
    });
  }

  function setHideEnded(on) {
    setPrefsAndRefresh(() => {
      prefs.hideEnded = on;
      // Off, every ended session is visible anyway; clearing shown means
      // turning it back on archives all of them again, not a remembered few.
      if (!on) prefs.shown = [];
    });
  }

  // Only Canvas.app's WKWebView injects window.__TAURI__ (withGlobalTauri in
  // tauri.conf.json); a plain browser tab never sees it, which is how the
  // button knows to stay hidden there.
  const keepOnTopEl = document.getElementById("keep-on-top-toggle");
  const tauriCore = window.__TAURI__ && window.__TAURI__.core;

  keepOnTopEl.appendChild(buildIcon("pin"));

  function applyKeepOnTop(on) {
    keepOnTopEl.setAttribute("aria-pressed", on ? "true" : "false");
    keepOnTopEl.setAttribute(
      "aria-label",
      on ? "Stop keeping window on top" : "Keep window on top"
    );
    setButtonIcon(keepOnTopEl, on ? "pin-fill" : "pin");
  }

  if (tauriCore) {
    keepOnTopEl.hidden = false;
    tauriCore
      .invoke("get_keep_on_top")
      .then((on) => applyKeepOnTop(!!on))
      .catch(() => {});

    keepOnTopEl.addEventListener("click", async () => {
      const nextOn = keepOnTopEl.getAttribute("aria-pressed") !== "true";
      try {
        await tauriCore.invoke("set_keep_on_top", { on: nextOn });
        applyKeepOnTop(nextOn);
      } catch (e) {
        // Refused invoke (wrong origin, missing capability) — leave the
        // button's displayed state as it was.
      }
    });
  }

  // The theme button sits between Keep on top and Sessions. The icon shows the mode
  // a click switches to. A change rebuilds every card, because a card's
  // iframe bakes the theme into its document.
  const themeToggleEl = document.getElementById("theme-toggle");

  function applyThemeButton() {
    const dark = window.canvasTheme.get() === "dark";
    themeToggleEl.setAttribute(
      "aria-label",
      dark ? "Switch to light mode" : "Switch to dark mode"
    );
    setButtonIcon(themeToggleEl, dark ? "sun" : "moon");
  }

  applyThemeButton();
  themeToggleEl.addEventListener("click", () => {
    window.canvasTheme.set(window.canvasTheme.get() === "dark" ? "light" : "dark");
  });
  window.addEventListener("canvas-theme", () => {
    applyThemeButton();
    bootstrapRender();
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

  // `owner/repo` on GitHub, or "" when the session's directory has none.
  function sessionRepo(sessionId) {
    const s = sessions.get(sessionId);
    return (s && s.repo) || "";
  }

  function sessionCardCount(sessionId) {
    let n = 0;
    for (const c of cards.values()) {
      if (c.sessionId === sessionId && !c.pin) n++;
    }
    return n;
  }

  // The chip's repo text: the name half of `owner/repo`, or with no repo the
  // last segment of the session's directory, marked local.
  function repoShort(session) {
    if (session.repo) return session.repo.slice(session.repo.indexOf("/") + 1);
    const dir = (session.cwd || "").replace(/\/+$/, "");
    return `${dir.slice(dir.lastIndexOf("/") + 1)} (local)`;
  }

  // The agent icon on a tile in the session colour, which the tile reads
  // from --session-colour on its chip or card.
  function buildAgentTile() {
    const tile = document.createElement("span");
    tile.className = "agent-tile";
    tile.title = "Claude Code";
    tile.appendChild(buildIcon("claude"));
    return tile;
  }

  // Isolates to exactly `id` (or, for null, resets to "everything visible")
  // regardless of whatever the current set already holds — distinct from
  // toggleSessionVisibility, which adds/removes one id from what's there.
  function selectSession(id) {
    visibleSessionIds = id === null ? null : new Set([id]);
    renderChips();
    applyFilter();
    renderDrawer();
    const pressed = chipsEl.querySelector('.chip[aria-pressed="true"]');
    if (pressed) pressed.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  // Flips one session's membership in the visible set. The first toggle
  // from "everything visible" isolates to just that one (state 1 -> 2);
  // every toggle after that adds or removes it from what's already there.
  // Toggling off the last one left empties the set back to null — "nothing
  // selected" is state 1 again, not a distinct "nothing visible" state.
  function toggleSessionVisibility(id) {
    if (visibleSessionIds === null) visibleSessionIds = new Set();
    if (visibleSessionIds.has(id)) visibleSessionIds.delete(id);
    else visibleSessionIds.add(id);
    if (visibleSessionIds.size === 0) visibleSessionIds = null;
    renderChips();
    applyFilter();
    renderDrawer();
  }

  function resetVisibility() {
    selectSession(null);
  }

  // Most recent post first; a session with no posts counts from when it
  // started.
  function byRecency(list) {
    const latest = new Map();
    for (const c of cards.values()) {
      const at = new Date(c.at).getTime();
      if (!(latest.get(c.sessionId) >= at)) latest.set(c.sessionId, at);
    }
    const lastActive = (s) => latest.get(s.id) ?? new Date(s.startedAt).getTime();
    return list.sort((a, b) => lastActive(b) - lastActive(a));
  }

  // Visible sessions that have posted, most recent post first.
  function chipSessions() {
    const posted = new Set();
    for (const c of cards.values()) posted.add(c.sessionId);
    return byRecency(
      Array.from(sessions.values()).filter((s) => !isHidden(s) && posted.has(s.id))
    );
  }

  // Ids of the chips in the last render, so the next one can tell which are
  // new; whether a chip new to this render enters with motion (true only
  // while a live card-upserted is being applied); and the chips still
  // mid-slide, which every render rebuilds and so must mark again.
  let renderedChipIds = new Set();
  let chipsMayArrive = false;
  const arrivingChipIds = new Set();

  // Rebuilding every chip on each render drops keyboard focus, since the
  // focused button leaves the document. Note which chip (by session id, ""
  // for All) had it so its replacement can take it back.
  function renderChips() {
    const active = document.activeElement;
    const focusedKey =
      active && chipsEl.contains(active) ? active.dataset.sessionId : null;

    const visible = chipSessions();
    let total = 0;
    for (const s of visible) total += sessionCardCount(s.id);

    const all = buildChip("", visibleSessionIds === null);
    all.classList.add("chip-all");
    all.append("All", chipCount(total));
    all.addEventListener("click", () => resetVisibility());

    const chips = [all];
    for (const s of visible) {
      const selected = visibleSessionIds !== null && visibleSessionIds.has(s.id);
      const chip = buildChip(s.id, selected);
      chip.style.setProperty("--session-colour", sessionColour(s));
      if (chipsMayArrive && animating() && !renderedChipIds.has(s.id)) {
        startChipArrival(s.id);
      }
      if (arrivingChipIds.has(s.id)) chip.dataset.arriving = "";
      chip.title = s.repo ? `${s.name} · ${s.repo}` : s.name;
      const name = document.createElement("span");
      name.className = "chip-name";
      name.textContent = s.name;
      const repo = document.createElement("span");
      repo.className = "chip-repo";
      repo.textContent = repoShort(s);
      const x = document.createElement("span");
      x.className = "chip-x";
      x.appendChild(buildIcon("x"));
      chip.append(buildAgentTile(), name, repo, chipCount(sessionCardCount(s.id)), x);
      chip.addEventListener("click", () => toggleSessionVisibility(s.id));
      chips.push(chip);
    }

    chipsEl.replaceChildren(...chips);
    renderedChipIds = new Set(visible.map((s) => s.id));
    if (focusedKey !== null) {
      const again = chips.find((c) => c.dataset.sessionId === focusedKey);
      if (again) again.focus();
    }
  }

  // A chip carries data-arriving for the 300ms slide. The timer, not
  // animationend, ends it, since a re-render replaces the chip element.
  function startChipArrival(sessionId) {
    arrivingChipIds.add(sessionId);
    setTimeout(() => {
      arrivingChipIds.delete(sessionId);
      const chip = chipsEl.querySelector(`.chip[data-session-id="${CSS.escape(sessionId)}"]`);
      if (chip) chip.removeAttribute("data-arriving");
    }, 300);
  }

  function buildChip(sessionId, selected) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip";
    chip.dataset.sessionId = sessionId;
    chip.setAttribute("aria-pressed", selected ? "true" : "false");
    return chip;
  }

  function chipCount(n) {
    const count = document.createElement("span");
    count.className = "chip-count";
    count.textContent = String(n);
    return count;
  }

  // The Sessions drawer docks beside the stream, narrowing it rather than
  // covering it, and lists every session, ACTIVE then ARCHIVED. Its header
  // stays put; the body is rebuilt on every change while the drawer is open.
  const drawerToggleEl = document.getElementById("drawer-toggle");
  const drawerBadgeEl = document.createElement("span");
  const drawerEl = document.getElementById("drawer");
  const drawerBodyEl = document.getElementById("drawer-body");
  drawerBadgeEl.className = "badge";
  drawerToggleEl.append(buildIcon("sidebar-right"), drawerBadgeEl);

  const settingsToggleEl = document.getElementById("settings-toggle");
  settingsToggleEl.appendChild(buildIcon("gear"));

  // Canvas.app opens the Settings window through its own command; a browser
  // tab gets a popup window on the same page, and reopening it focuses the
  // one already there.
  settingsToggleEl.addEventListener("click", () => {
    if (window.__TAURI__ && window.__TAURI__.core) {
      window.__TAURI__.core.invoke("open_settings").catch(() => {});
    } else {
      window.open("/settings.html", "canvas-settings", "popup,width=640,height=560");
    }
  });

  function drawerIsOpen() {
    return drawerEl.classList.contains("open");
  }

  function openDrawer() {
    drawerEl.classList.add("open");
    drawerEl.inert = false;
    drawerToggleEl.setAttribute("aria-expanded", "true");
    renderDrawer();
  }

  function closeDrawer({ refocus = false } = {}) {
    if (!drawerIsOpen()) return;
    if (pendingConfirm && drawerEl.contains(pendingConfirm.button)) {
      clearPendingConfirm();
    }
    drawerEl.classList.remove("open");
    drawerEl.inert = true;
    drawerToggleEl.setAttribute("aria-expanded", "false");
    if (refocus) drawerToggleEl.focus();
  }

  drawerToggleEl.addEventListener("click", () => {
    if (drawerIsOpen()) closeDrawer();
    else openDrawer();
  });

  // The first Escape only reverts an armed confirm (its own handler below);
  // the drawer stays until the next one.
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || !drawerIsOpen() || lightbox) return;
    if (pendingConfirm && drawerEl.contains(pendingConfirm.button)) return;
    closeDrawer({ refocus: true });
  });

  // A click inside a card's iframe never reaches this document; focus
  // moving into that iframe is the sign. Switching apps also blurs the
  // window but leaves activeElement outside any iframe, so the drawer stays.
  window.addEventListener("blur", () => {
    if (document.activeElement instanceof HTMLIFrameElement) closeDrawer();
  });

  // Icon-only, fixed-size button. `key` names it across a rebuild so an armed
  // confirm and keyboard focus can find their replacement.
  function drawerButton(key, icon, label, onClick, danger) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = danger ? "icon-btn danger" : "icon-btn";
    btn.dataset.key = key;
    btn.setAttribute("aria-label", label);
    btn.title = label;
    btn.appendChild(buildIcon(icon));
    btn.addEventListener("click", onClick);
    return btn;
  }

  // A section heading's bulk action: a labelled button rather than the
  // per-row icon-only style, since there's only one and it needs to read as
  // a verb at a glance. Disabled — never hidden — when there's nothing for
  // it to do, so it never shifts position in the header.
  function drawerActionButton(key, label, onClick, disabled) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "drawer-action-btn";
    btn.dataset.key = key;
    setButtonIcon(btn, null, label);
    btn.disabled = disabled;
    btn.addEventListener("click", onClick);
    return btn;
  }

  // Two clicks: the first swaps the icon for a check, the second runs it.
  function confirmButton(key, icon, label, confirmLabel, onConfirm) {
    const btn = drawerButton(
      key,
      icon,
      label,
      () =>
        armConfirm(btn, {
          idleLabel: label,
          idleIcon: icon,
          confirmLabel,
          confirmIcon: "check",
          onConfirm,
        }),
      true
    );
    return btn;
  }

  function buildSessionRow(session, section) {
    const row = document.createElement("div");
    row.className = "drawer-row";
    row.dataset.sessionId = session.id;
    row.dataset.section = section;

    const dot = document.createElement("span");
    dot.className = "session-dot";
    dot.style.setProperty("--session-colour", sessionColour(session));

    const text = document.createElement("div");
    text.className = "drawer-row-text";
    const name = document.createElement("div");
    name.className = "drawer-row-name";
    name.textContent = session.name;
    const meta = document.createElement("small");
    const count = sessionCardCount(session.id);
    const parts = [`${count} post${count === 1 ? "" : "s"}`];
    if (session.repo) parts.unshift(session.repo);
    if (section === "archived" && session.endedAt) {
      parts.push(`ended ${relativeTime(session.endedAt)}`);
    }
    meta.textContent = parts.join(" · ");
    const id = document.createElement("small");
    id.className = "drawer-row-id";
    id.textContent = session.id.slice(0, 8);
    id.title = session.id;
    text.append(name, meta, id);
    row.append(dot, text);

    if (section === "active") {
      // The icon tracks effective visibility (on in state 1, since nothing
      // is excluded yet); aria-pressed only lights up once this session is
      // actually named in a custom selection, matching the chip it shares
      // state with.
      const isSelected = visibleSessionIds !== null && visibleSessionIds.has(session.id);
      const isVisible = visibleSessionIds === null || isSelected;
      const visibilityToggle = drawerButton(
        `visible:${session.id}`,
        isVisible ? "eye" : "eye-off",
        isVisible ? "Showing in stream — click to isolate or hide" : "Hidden — click to show",
        () => toggleSessionVisibility(session.id)
      );
      visibilityToggle.setAttribute("aria-pressed", String(isSelected));
      row.append(
        visibilityToggle,
        confirmButton(
          `archive:${session.id}`,
          "archive",
          "Archive (deletes its posts)",
          "Really archive & delete its posts?",
          () => archiveSession(session.id)
        )
      );
    } else {
      row.append(
        drawerButton(`show:${session.id}`, "unarchive", "Show", () => {
          showSession(session.id);
          toast(`Showing ${session.name}`);
        }),
        confirmButton(
          `delete:${session.id}`,
          "trash",
          "Delete",
          "Confirm delete",
          () => deleteSession(session.id)
        )
      );
    }
    return row;
  }

  function drawerSection(section, title, list, action) {
    const el = document.createElement("section");
    el.className = "drawer-section";
    el.dataset.section = section;
    const head = document.createElement("div");
    head.className = "drawer-heading";
    const h = document.createElement("h3");
    h.textContent = `${title} (${list.length})`;
    head.appendChild(h);
    if (action) head.appendChild(action);
    el.appendChild(head);
    if (list.length === 0) {
      const none = document.createElement("div");
      none.className = "drawer-empty";
      none.textContent = `No ${section} sessions`;
      el.appendChild(none);
    }
    for (const s of list) el.appendChild(buildSessionRow(s, section));
    return el;
  }

  function renderDrawer() {
    const archived = archivedSessions();
    drawerBadgeEl.textContent = String(archived.length);
    drawerBadgeEl.hidden = archived.length === 0;
    if (!drawerIsOpen()) return;

    // A rebuild detaches an armed button, so clear it and re-arm its
    // replacement with the time it had left; a post arriving mid-confirm
    // doesn't cancel it.
    let rearm = null;
    if (pendingConfirm && drawerBodyEl.contains(pendingConfirm.button)) {
      rearm = {
        key: pendingConfirm.button.dataset.key,
        opts: pendingConfirm.opts,
        ms: pendingConfirm.deadline - Date.now(),
      };
      clearPendingConfirm();
    }
    const active = document.activeElement;
    const focusedKey =
      active && drawerBodyEl.contains(active) ? active.dataset.key : null;
    const scrollTop = drawerBodyEl.scrollTop;

    const activeList = byRecency(Array.from(sessions.values()).filter((x) => !isHidden(x)));
    const archivedList = byRecency(archived);
    const showAll = drawerActionButton(
      "show-all",
      "Show all",
      () => resetVisibility(),
      visibleSessionIds === null
    );
    const clearAll = drawerActionButton(
      "clear-all",
      "Clear all",
      () =>
        armConfirm(clearAll, {
          idleLabel: "Clear all",
          idleIcon: null,
          confirmLabel: "Confirm",
          confirmIcon: null,
          withText: true,
          onConfirm: () => {
            // The archive as it is at the second click, not at render time.
            for (const s of archivedSessions()) deleteSession(s.id);
          },
        }),
      archivedList.length === 0
    );
    clearAll.classList.add("danger");
    drawerBodyEl.replaceChildren(
      drawerSection("active", "Active", activeList, showAll),
      drawerSection("archived", "Archived", archivedList, clearAll)
    );
    drawerBodyEl.scrollTop = scrollTop;

    const byKey = (key) => drawerBodyEl.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (rearm && rearm.ms > 0) {
      const btn = byKey(rearm.key);
      if (btn) armConfirm(btn, rearm.opts, rearm.ms);
    }
    if (focusedKey) {
      const btn = byKey(focusedKey);
      if (btn) btn.focus();
    }
  }

  function textButton(text, className, onClick) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = className;
    btn.textContent = text;
    btn.addEventListener("click", onClick);
    return btn;
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

  // `withText: true` shows the label beside the icon (a menu item) instead
  // of only in aria-label (an icon button).
  function armConfirm(button, opts, ms = 4000) {
    const { idleLabel, idleIcon, confirmLabel, confirmIcon, onConfirm, withText } = opts;
    if (pendingConfirm && pendingConfirm.button === button) {
      clearPendingConfirm();
      onConfirm();
      return;
    }
    clearPendingConfirm();
    setButtonIcon(button, confirmIcon, withText ? confirmLabel : undefined);
    button.setAttribute("aria-label", confirmLabel);
    if (button.title) button.title = confirmLabel;
    button.classList.add("confirming");
    const timeoutId = setTimeout(clearPendingConfirm, ms);
    pendingConfirm = {
      button,
      opts,
      deadline: Date.now() + ms,
      timeoutId,
      revert() {
        setButtonIcon(button, idleIcon, withText ? idleLabel : undefined);
        button.setAttribute("aria-label", idleLabel);
        if (button.title) button.title = idleLabel;
        button.classList.remove("confirming");
      },
    };
  }

  // composedPath() is fixed when the event is dispatched, so a click on a
  // button whose contents were rebuilt mid-dispatch (armConfirm swapping the
  // icon under the pointer) still counts as inside it; e.target no longer
  // would, having been detached.
  function clickIsInside(e, el) {
    return e.composedPath().includes(el);
  }

  document.addEventListener("click", (e) => {
    if (pendingConfirm && !clickIsInside(e, pendingConfirm.button)) {
      clearPendingConfirm();
    }
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && pendingConfirm) {
      clearPendingConfirm();
    }
  });

  // Bottom-left toasts: a message and at most one action button. Each lasts
  // 3s (6s with an action); a fourth pushes the oldest out.
  const MAX_TOASTS = 3;

  function toast(message, action, onAction) {
    const el = document.createElement("div");
    el.className = "toast";
    const text = document.createElement("span");
    text.textContent = message;
    el.appendChild(text);
    if (action) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "toast-action";
      btn.textContent = action;
      btn.addEventListener("click", () => {
        el.remove();
        onAction();
      });
      el.appendChild(btn);
    }
    toastsEl.appendChild(el);
    while (toastsEl.children.length > MAX_TOASTS) {
      toastsEl.firstChild.remove();
    }
    setTimeout(() => el.remove(), action ? 6000 : 3000);
  }

  // The text a reader sees in a post, from card.html alone. DOMParser builds
  // an inert document that is never attached here — no script runs and no
  // image loads — and <style>/<script> are dropped so the CSS a Markdown
  // post carries never reaches the text. Whitespace follows rendering: runs
  // collapse to one space outside <pre>, block elements end a line, and
  // table cells are tab-separated. Copy post text and search share it.
  const BLOCK_TAGS =
    "address, article, aside, blockquote, dd, div, dl, dt, figcaption, figure, " +
    "footer, h1, h2, h3, h4, h5, h6, header, hr, li, main, nav, ol, p, pre, " +
    "section, table, tr, ul";

  function postText(card) {
    const doc = new DOMParser().parseFromString(card.html, "text/html");
    for (const el of doc.querySelectorAll("style, script, template, noscript")) {
      el.remove();
    }
    const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (!node.parentElement.closest("pre, textarea")) {
        node.data = node.data.replace(/\s+/g, " ");
      }
    }
    for (const el of doc.body.querySelectorAll("br")) el.replaceWith("\n");
    for (const el of doc.body.querySelectorAll("td, th")) el.after("\t");
    for (const el of doc.body.querySelectorAll(BLOCK_TAGS)) el.after("\n");
    return doc.body.textContent
      .replace(/[ \t]+$/gm, "")
      .replace(/^ (?=\S)/gm, "")
      .replace(/\n{3,}/g, "\n\n")
      .trim();
  }

  // The one open card menu, if any: { menu, button }.
  let openMenu = null;

  function closeMenu({ refocus = false } = {}) {
    if (!openMenu) return;
    const { menu, button } = openMenu;
    openMenu = null;
    if (pendingConfirm && menu.contains(pendingConfirm.button)) {
      clearPendingConfirm();
    }
    menu.remove();
    button.setAttribute("aria-expanded", "false");
    if (refocus) button.focus();
  }

  // A card re-render or removal can detach the open menu with its card.
  function closeMenuIfDetached() {
    if (openMenu && !openMenu.menu.isConnected) closeMenu();
  }

  function buildMenuItem(iconName, text, onClick) {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "menu-item";
    item.setAttribute("role", "menuitem");
    item.tabIndex = -1;
    setButtonIcon(item, iconName, text);
    item.addEventListener("click", onClick);
    return item;
  }

  function copyPostText(card) {
    closeMenu();
    navigator.clipboard.writeText(postText(card)).then(
      () => toast("Copied post text"),
      () => toast("Couldn't copy post text")
    );
  }

  // The link Canvas.app registers as a URL scheme; opening it brings the
  // post up (openCardLink), and `canvas card <id>` prints the post.
  function copyPostLink(card) {
    closeMenu();
    navigator.clipboard.writeText(`canvas-post://${card.id}`).then(
      () => toast("Copied post link"),
      () => toast("Couldn't copy post link")
    );
  }

  function showOnlySession(sessionId) {
    closeMenu();
    selectSession(sessionId);
    streamEl.scrollTop = 0;
  }

  function toggleCardMenu(card, header, button) {
    const wasOpenHere = openMenu && openMenu.button === button;
    closeMenu();
    if (wasOpenHere) return;

    const menu = document.createElement("div");
    menu.className = "card-menu";
    menu.setAttribute("role", "menu");
    menu.setAttribute("aria-label", "Post actions");

    menu.appendChild(buildMenuItem("copy", "Copy post text", () => copyPostText(card)));
    menu.appendChild(buildMenuItem("link", "Copy post link", () => copyPostLink(card)));
    menu.appendChild(
      buildMenuItem("filter", `Show only ${sessionName(card.sessionId)}`, () =>
        showOnlySession(card.sessionId)
      )
    );
    const divider = document.createElement("div");
    divider.className = "menu-divider";
    divider.setAttribute("role", "separator");
    menu.appendChild(divider);
    const archiveItem = buildMenuItem("archive", "Archive this session", () => {
      armConfirm(archiveItem, {
        idleLabel: "Archive this session",
        idleIcon: "archive",
        confirmLabel: "Really archive & delete its posts?",
        confirmIcon: "archive",
        withText: true,
        onConfirm: () => {
          closeMenu();
          archiveSession(card.sessionId);
        },
      });
    });
    archiveItem.classList.add("danger");
    menu.appendChild(archiveItem);
    const deleteItem = buildMenuItem("trash", "Delete post", () => {
      armConfirm(deleteItem, {
        idleLabel: "Delete post",
        idleIcon: "trash",
        confirmLabel: "Confirm delete",
        confirmIcon: "trash",
        withText: true,
        onConfirm: () => {
          closeMenu();
          deleteCard(card.id);
        },
      });
    });
    deleteItem.classList.add("danger");
    menu.appendChild(deleteItem);

    menu.addEventListener("keydown", (e) => {
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      e.preventDefault();
      const items = Array.from(menu.querySelectorAll('[role="menuitem"]'));
      const step = e.key === "ArrowDown" ? 1 : -1;
      const at = items.indexOf(document.activeElement);
      items[(at + step + items.length) % items.length].focus();
    });

    // Items have tabIndex -1, so Tab leaves the menu. relatedTarget is null
    // for that Tab in Chromium, so check where focus is once the move is done.
    // Window blur leaves activeElement inside the menu, so the menu stays.
    menu.addEventListener("focusout", () => {
      setTimeout(() => {
        if (!openMenu || openMenu.menu !== menu) return;
        const active = document.activeElement;
        if (!menu.contains(active) && active !== button) closeMenu();
      }, 0);
    });

    header.appendChild(menu);
    button.setAttribute("aria-expanded", "true");
    openMenu = { menu, button };
    menu.querySelector('[role="menuitem"]').focus();
  }

  document.addEventListener("click", (e) => {
    if (
      openMenu &&
      !clickIsInside(e, openMenu.menu) &&
      !clickIsInside(e, openMenu.button)
    ) {
      closeMenu();
    }
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && openMenu) closeMenu({ refocus: true });
  });

  // A click inside a card's iframe never reaches this document; focus
  // moving into that iframe is the sign, so treat that as an outside click.
  // Switching apps also blurs the window but leaves activeElement outside
  // any card iframe, so the menu stays.
  window.addEventListener("blur", () => {
    const active = document.activeElement;
    if (
      active instanceof HTMLIFrameElement &&
      (cardsEl.contains(active) || (sheet && sheet.dialog.contains(active)))
    ) {
      closeMenu();
    }
  });

  function deleteCard(id) {
    fetch(`/api/cards/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
  }

  function clearSessionPosts(id) {
    fetch(`/api/sessions/${encodeURIComponent(id)}/cards`, { method: "DELETE" }).catch(() => {});
  }

  function deleteSession(id) {
    fetch(`/api/sessions/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
  }

  // The server is the source of truth for removal: these run only once the
  // card-removed / session-removed SSE event arrives, so every open viewer
  // (including the one that clicked Confirm) stays in sync the same way.
  function removeCard(id) {
    if (!cards.has(id)) return;
    if (lightbox && lightbox.card.id === id) closeLightbox();
    cards.delete(id);
    postTextLower.delete(id);
    cardDataPushes.delete(id);
    unseen.cardIds.delete(id);
    removeWidget(id);
    const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(id)}"]`);
    if (el) {
      // A card removed from above the reader shrinks the stream above them
      // exactly as a resize does — hold place against its own height going
      // away, before it leaves the document.
      const wasAbove = cardIsAboveViewport(el);
      const height = el.offsetHeight + (parseFloat(getComputedStyle(el).marginBottom) || 0);
      el.remove();
      if (wasAbove) holdPlace(-height);
    }
    closeMenuIfDetached();
    refreshVisibility();
  }

  function removeSession(id) {
    if (!sessions.has(id)) return;
    if (lightbox && lightbox.card.sessionId === id) closeLightbox();
    const name = sessionName(id);
    sessions.delete(id);
    for (const [cardId, c] of Array.from(cards.entries())) {
      if (c.sessionId !== id) continue;
      cards.delete(cardId);
      postTextLower.delete(cardId);
      unseen.cardIds.delete(cardId);
      removeWidget(cardId);
      const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(cardId)}"]`);
      if (el) {
        const wasAbove = cardIsAboveViewport(el);
        const height = el.offsetHeight + (parseFloat(getComputedStyle(el).marginBottom) || 0);
        el.remove();
        if (wasAbove) holdPlace(-height);
      }
    }
    closeMenuIfDetached();
    prunePrefs();
    refreshVisibility();
    toast(`Deleted ${name} and its posts`);
  }

  // Search. `query` is the trimmed field text; a card matches when its
  // lower-cased text, session name, repo or directory contains it. postText
  // parses the whole post, so its result is kept per card id and dropped when
  // the card is replaced or removed.
  let query = "";
  const searchInputEl = document.getElementById("search-input");
  const searchEl = document.getElementById("search");
  const searchCountEl = document.getElementById("search-count");
  const searchHintEl = document.getElementById("search-hint");
  const searchClearEl = document.getElementById("search-clear");
  const postTextLower = new Map(); // card id -> lower-cased postText

  document.getElementById("search-icon").appendChild(buildIcon("search"));
  searchClearEl.appendChild(buildIcon("x"));

  function cardTextLower(card) {
    let text = postTextLower.get(card.id);
    if (text === undefined) {
      text = postText(card).toLowerCase();
      postTextLower.set(card.id, text);
    }
    return text;
  }

  function cardMatchesQuery(card) {
    if (query === "") return true;
    const q = query.toLowerCase();
    const session = sessions.get(card.sessionId);
    if (session) {
      for (const field of [session.name, session.repo, session.cwd]) {
        if (field && field.toLowerCase().includes(q)) return true;
      }
    }
    return cardTextLower(card).includes(q);
  }

  function cardMatchesFilter(card) {
    const session = sessions.get(card.sessionId);
    if (session && isHidden(session)) return false;
    if (visibleSessionIds !== null && !visibleSessionIds.has(card.sessionId)) return false;
    return cardMatchesQuery(card);
  }

  // Marks inside the posts follow the query once it is two characters long;
  // anything shorter clears them. Each iframe is told only when this string
  // changes, and once more as it loads.
  let highlightQuery = "";

  function postHighlight(frame) {
    if (frame.contentWindow) {
      frame.contentWindow.postMessage({ type: "canvas-highlight", query: highlightQuery }, "*");
    }
  }

  function syncHighlight() {
    const next = query.length >= 2 ? query : "";
    if (next === highlightQuery) return;
    highlightQuery = next;
    for (const frame of cardFrames()) postHighlight(frame);
  }

  function setQuery(value) {
    query = value.trim();
    searchInputEl.value = value;
    const hasText = value !== "";
    searchHintEl.hidden = hasText;
    searchClearEl.hidden = !hasText;
    syncHighlight();
    applyFilter();
  }

  function clearSearch() {
    resetVisibility();
    setQuery("");
  }

  searchInputEl.addEventListener("input", () => setQuery(searchInputEl.value));
  searchClearEl.addEventListener("click", () => {
    setQuery("");
    searchInputEl.focus();
  });
  searchInputEl.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (searchInputEl.value !== "") setQuery("");
    else searchInputEl.blur();
  });
  window.addEventListener("keydown", (e) => {
    if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
    const t = e.target;
    if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t.isContentEditable) {
      return;
    }
    e.preventDefault();
    searchInputEl.focus();
    searchInputEl.select();
  });

  // Hide/show existing card elements per the current filter without
  // touching any other card's DOM node (and any live iframe inside it).
  function applyFilter() {
    let visible = 0;
    let fromVisibleSessions = 0;
    for (const el of cardsEl.children) {
      const card = cards.get(el.dataset.cardId);
      const matches = cardMatchesFilter(card);
      el.hidden = !matches;
      if (matches) visible++;
      const session = sessions.get(card.sessionId);
      if (!session || !isHidden(session)) fromVisibleSessions++;
    }
    streamEl.dataset.filterSession = visibleSessionIds ? Array.from(visibleSessionIds).join(" ") : "";
    streamEl.dataset.query = query;
    const filtering = visibleSessionIds !== null || query !== "";
    searchCountEl.hidden = !filtering;
    searchCountEl.textContent = `${visible} of ${fromVisibleSessions}`;
    searchEl.classList.toggle("filtering", filtering);
    // A shelf of pins is not an empty window.
    updateEmptyState(visible || (shelf && !filtering ? 1 : 0));
    renderPill();
  }

  // The empty-state message differs depending on whether every session is
  // archived, the selected chip has no posts, or there are no posts at all.
  function updateEmptyState(visible) {
    emptyStateEl.hidden = visible > 0;
    emptyStateEl.classList.remove("hero");
    if (visible > 0) return;
    const sessionList = Array.from(sessions.values());
    if (sessionList.length > 0 && sessionList.every(isHidden)) {
      // "show ended sessions" only helps when the ended rule archived one
      // of them; sessions archived by hand come back one at a time in the
      // Sessions drawer.
      const endedArchived = sessionList.some((s) => !prefs.hidden.includes(s.id));
      if (endedArchived) {
        emptyStateEl.replaceChildren(
          "No open sessions. Posts from sessions that ended are kept in the archive — ",
          textButton("show ended sessions", "link-btn", () => setHideEnded(false)),
          "."
        );
      } else {
        emptyStateEl.replaceChildren(
          "All sessions are archived. ",
          // Stopped here, or the page-wide outside-click handler sees a
          // click outside the drawer it just opened and closes it again.
          textButton("Open Sessions", "link-btn", (e) => {
            e.stopPropagation();
            openDrawer();
          })
        );
      }
    } else if (query !== "") {
      emptyStateEl.replaceChildren(
        `No posts match “${query}”. `,
        textButton("Clear search", "link-btn", clearSearch)
      );
    } else if (visibleSessionIds !== null) {
      const label = visibleSessionIds.size === 1 ? "this session" : "these sessions";
      emptyStateEl.replaceChildren(
        `No posts from ${label} yet. `,
        textButton("Show all", "link-btn", resetVisibility)
      );
    } else {
      emptyStateEl.classList.add("hero");
      emptyStateEl.replaceChildren(buildFirstRunEmpty());
    }
  }

  // Three offset cards, the front one with a few lines of "content".
  function buildEmptyIllustration() {
    const svg = svgEl("svg", {
      viewBox: "0 0 120 96",
      width: "120",
      height: "96",
      class: "empty-art",
      "aria-hidden": "true",
      focusable: "false",
    });
    svg.appendChild(svgEl("rect", { x: "22", y: "6", width: "76", height: "56", rx: "8", class: "art-back2" }));
    svg.appendChild(svgEl("rect", { x: "14", y: "16", width: "92", height: "60", rx: "8", class: "art-back1" }));
    svg.appendChild(svgEl("rect", { x: "6", y: "28", width: "108", height: "62", rx: "8", class: "art-front" }));
    svg.appendChild(svgEl("circle", { cx: "20", cy: "42", r: "4", class: "art-dot" }));
    svg.appendChild(svgEl("rect", { x: "30", y: "39", width: "44", height: "6", rx: "3", class: "art-line strong" }));
    svg.appendChild(svgEl("rect", { x: "18", y: "56", width: "84", height: "5", rx: "2.5", class: "art-line" }));
    svg.appendChild(svgEl("rect", { x: "18", y: "68", width: "58", height: "5", rx: "2.5", class: "art-line" }));
    return svg;
  }

  // The no-posts-at-all state: what Canvas is and how the first post gets here.
  function buildFirstRunEmpty() {
    const wrap = document.createElement("div");
    wrap.className = "empty-hero";
    const title = document.createElement("h2");
    title.className = "empty-title";
    title.textContent = "Nothing posted yet";
    const body = document.createElement("p");
    body.className = "empty-body";
    body.textContent =
      "Plans, docs and images your Claude Code sessions post appear here as they work.";
    const hint = document.createElement("div");
    hint.className = "empty-hint";
    const ask = document.createElement("span");
    ask.textContent = "Ask a session:";
    const prompt = document.createElement("code");
    prompt.textContent = "post this plan to Canvas";
    const or = document.createElement("span");
    or.className = "empty-or";
    or.textContent = "or run";
    const cmd = document.createElement("code");
    cmd.textContent = "canvas post notes.md";
    hint.append(ask, prompt, or, cmd);
    wrap.append(buildEmptyIllustration(), title, body, hint);
    return wrap;
  }

  // A srcdoc iframe (sandbox="allow-scripts", no allow-same-origin) has an
  // opaque origin, so a relative `src="/api/cards/..."` inside it never
  // resolves to the daemon at all — no request is even attempted, and the
  // image shows as broken. canvasd only ever writes that one path shape
  // into post HTML, so making exactly that shape absolute to the daemon's
  // own origin (both quote styles) is enough; nothing else in the HTML is
  // touched.
  function absolutizeCardImageSrcs(html) {
    // scan.rs preserves the source HTML's attribute-name casing (it only
    // rewrites the value, not `tag[..vs]`) — an `<IMG SRC="...">` post
    // reaches here with `SRC` still uppercase, so the attribute name must
    // be matched case-insensitively even though canvasd only ever writes
    // lowercase `src` itself.
    return html.replace(
      /(src=["'])(\/api\/cards\/)/gi,
      (_match, prefix, path) => `${prefix}${location.origin}${path}`
    );
  }

  // A card renders in the window's theme whatever it asks for. Its
  // `prefers-color-scheme` media queries are rewritten so only the active
  // theme's block matches (`(min-width:0px)` is always true, the huge
  // `min-width` never is, and both stay valid after `and`), and the injected
  // script answers `matchMedia` for the same queries. A stylesheet loaded
  // from a CDN is out of reach; the forced surface, ink and contrast pass
  // still keep its text readable.
  function forceCardTheme(html, theme) {
    return html.replace(
      /\(\s*prefers-color-scheme\s*:\s*(dark|light)\s*\)/gi,
      (_m, want) =>
        want.toLowerCase() === theme ? "(min-width:0px)" : "(min-width:999999px)"
    );
  }

  function buildIframeDoc(html) {
    const theme = window.canvasTheme.get();
    const rootStyle = getComputedStyle(document.documentElement);
    const surface = rootStyle.getPropertyValue("--surface").trim();
    const ink = rootStyle.getPropertyValue("--fg").trim();
    html = forceCardTheme(absolutizeCardImageSrcs(html), theme);
    const csp =
      "default-src 'none'; " +
      "script-src https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com 'unsafe-inline'; " +
      "style-src 'unsafe-inline' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com https://fonts.googleapis.com; " +
      "font-src https://fonts.gstatic.com data:; " +
      "img-src * data: canvas:;";
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
        // A post's own <style> can pad or margin body and html (the wrapper
        // sits inside them). The wrapper's bottom edge measured from the
        // top of the document already carries everything above it; what
        // sits below it is added back.
        function canvasBelow(el) {
          var s = getComputedStyle(el);
          return parseFloat(s.marginBottom) + parseFloat(s.paddingBottom) +
            parseFloat(s.borderBottomWidth);
        }
        function canvasSendHeight() {
          var h = Math.ceil(canvasRoot.getBoundingClientRect().bottom + window.pageYOffset +
            canvasBelow(document.body) + canvasBelow(document.documentElement));
          // Safety net: if the document is taller than the iframe's viewport,
          // the post is being clipped whatever the wrapper measurement said.
          // scrollHeight is only trusted when it exceeds the viewport, so it
          // cannot hold the height up after the content shrinks.
          var se = document.scrollingElement || document.documentElement;
          if (se.scrollHeight > window.innerHeight + 1) h = Math.max(h, se.scrollHeight);
          parent.postMessage({ type: 'canvas-resize', height: h }, '*');
        }
        window.addEventListener('load', canvasSendHeight);
        window.addEventListener('resize', canvasSendHeight);
        try {
          new ResizeObserver(canvasSendHeight).observe(canvasRoot);
        } catch (e) {}
        setInterval(canvasSendHeight, 400);

        // The parent's search query, marked in every text node. The old marks
        // go first; an empty query leaves the post as it was.
        function canvasHighlight(query) {
          var marks = canvasRoot.querySelectorAll('mark[data-canvas-hit]');
          for (var i = 0; i < marks.length; i++) {
            var mark = marks[i];
            var holder = mark.parentNode;
            holder.replaceChild(document.createTextNode(mark.textContent), mark);
            holder.normalize();
          }
          if (!query) return;
          var q = query.toLowerCase();
          var walker = document.createTreeWalker(canvasRoot, NodeFilter.SHOW_TEXT, {
            acceptNode: function (n) {
              return n.parentElement && n.parentElement.closest('script, style, noscript, template, textarea, .__cv-head, .__cv-more, .__cv-linkbar')
                ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;
            }
          });
          var nodes = [];
          while (walker.nextNode()) nodes.push(walker.currentNode);
          nodes.forEach(function (node) {
            var text = node.data;
            var lower = text.toLowerCase();
            var at = lower.indexOf(q);
            if (at < 0) return;
            var frag = document.createDocumentFragment();
            var last = 0;
            while (at >= 0) {
              if (at > last) frag.appendChild(document.createTextNode(text.slice(last, at)));
              var mark = document.createElement('mark');
              mark.setAttribute('data-canvas-hit', '');
              mark.textContent = text.slice(at, at + q.length);
              frag.appendChild(mark);
              last = at + q.length;
              at = lower.indexOf(q, last);
            }
            if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
            node.parentNode.replaceChild(frag, node);
          });
        }
        window.addEventListener('message', function (e) {
          if (e.source !== parent) return;
          var d = e.data;
          if (d && d.type === 'canvas-highlight' && typeof d.query === 'string') {
            canvasHighlight(d.query);
            canvasSendHeight();
          }
        });

        // The scan step (canvas post) rewrites a local path or http(s) link
        // to '#canvas-open-<n>' and records the real target server-side, at
        // card.targets[n] — this iframe never learns or names a real path,
        // it only ever sends the index it clicked.
        document.addEventListener('click', function (e) {
          var el = e.target.closest && e.target.closest('a[href^="#canvas-open-"]');
          if (!el) return;
          e.preventDefault();
          var n = parseInt(el.getAttribute('href').slice('#canvas-open-'.length), 10);
          if (Number.isNaN(n)) return;
          parent.postMessage({ type: 'canvas-open', index: n }, '*');
        });

        // Hovering or focusing such a link shows a bar with Open and Copy.
        // The parent says which kind each index is (kinds only, never the
        // targets), so the bar can word its buttons.
        var canvasKinds = [];
        var canvasBar = null, canvasBarTimer = null, canvasBarLink = null;
        function canvasReportBar(open) {
          parent.postMessage({ type: 'canvas-linkbar', open: open }, '*');
        }
        function canvasHideBar() {
          clearTimeout(canvasBarTimer);
          if (!canvasBar) return;
          canvasBar.remove();
          canvasBar = null;
          canvasBarLink = null;
          canvasReportBar(false);
        }
        function canvasHideSoon() {
          clearTimeout(canvasBarTimer);
          canvasBarTimer = setTimeout(canvasHideBar, 250);
        }
        function canvasLinkIndex(a) {
          return parseInt(a.getAttribute('href').slice('#canvas-open-'.length), 10);
        }
        function canvasShowBar(a) {
          var n = canvasLinkIndex(a);
          var kind = canvasKinds[n];
          if (Number.isNaN(n) || (kind !== 'path' && kind !== 'url')) return;
          if (canvasBarLink === a) { clearTimeout(canvasBarTimer); return; }
          canvasHideBar();
          var bar = document.createElement('div');
          bar.className = '__cv-linkbar';
          [['open', kind === 'url' ? 'Open in browser' : 'Open'],
           ['copy', kind === 'url' ? 'Copy link' : 'Copy path']].forEach(function (b) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.textContent = b[1];
            btn.addEventListener('click', function () {
              parent.postMessage({ type: b[0] === 'open' ? 'canvas-open' : 'canvas-copy-target', index: n }, '*');
              canvasHideBar();
            });
            bar.appendChild(btn);
          });
          bar.addEventListener('mouseenter', function () { clearTimeout(canvasBarTimer); });
          bar.addEventListener('mouseleave', canvasHideSoon);
          bar.addEventListener('focusout', function (e) {
            if (e.relatedTarget && (bar.contains(e.relatedTarget) || e.relatedTarget === a)) return;
            canvasHideSoon();
          });
          // Right after the link in tab order, so Tab from the link reaches
          // the buttons; fixed, so it takes no space in the post's layout.
          a.parentNode.insertBefore(bar, a.nextSibling);
          var r = a.getBoundingClientRect();
          var top = r.top - bar.offsetHeight - 6;
          if (top < 2) top = r.bottom + 6;
          bar.style.left = Math.max(4, Math.min(r.left, window.innerWidth - bar.offsetWidth - 4)) + 'px';
          bar.style.top = top + 'px';
          canvasBar = bar;
          canvasBarLink = a;
          canvasReportBar(true);
        }
        function canvasBarLinkOf(e) {
          return e.target.closest && e.target.closest('a[href^="#canvas-open-"]');
        }
        document.addEventListener('mouseover', function (e) {
          var a = canvasBarLinkOf(e);
          if (a) canvasShowBar(a);
        });
        document.addEventListener('mouseout', function (e) {
          if (canvasBarLinkOf(e)) canvasHideSoon();
        });
        document.addEventListener('focusin', function (e) {
          var a = canvasBarLinkOf(e);
          if (a) canvasShowBar(a);
        });
        document.addEventListener('focusout', function (e) {
          if (!canvasBarLinkOf(e)) return;
          // Focus moving into the bar keeps it; anywhere else hides it.
          var next = e.relatedTarget;
          if (next && canvasBar && canvasBar.contains(next)) return;
          canvasHideSoon();
        });
        window.addEventListener('scroll', canvasHideBar, true);
        window.addEventListener('message', function (e) {
          if (e.source !== parent) return;
          var d = e.data;
          if (!d) return;
          if (d.type === 'canvas-target-kinds' && Array.isArray(d.kinds)) canvasKinds = d.kinds;
          if (d.type === 'canvas-hide-linkbar') canvasHideBar();
        });

        // A card image — its src absolutized above to
        // '<origin>/api/cards/<id>/images/<n>' — opens in the viewer's
        // lightbox. Like a link, it sends only the index n; an image inside a
        // link belongs to the link.
        document.addEventListener('click', function (e) {
          var img = e.target.closest && e.target.closest('img');
          if (!img || img.closest('a[href]')) return;
          var m = /\\/api\\/cards\\/[^\\/]+\\/images\\/(\\d+)$/.exec(img.getAttribute('src') || '');
          if (!m) return;
          parent.postMessage({ type: 'canvas-image', index: parseInt(m[1], 10) }, '*');
        });

        // Code blocks: each <pre> gets a header (language, Copy) and, past
        // 14 lines, a clamp with a Show all / Collapse bar. The index n is
        // the block's position among the post's <pre> elements, taken before
        // any wrapping. Copy sends only n; the parent reads the text from
        // its own parse of card.html.
        var CANVAS_CLAMP_LINES = 14;
        var CANVAS_ICON_COPY = '<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 5.5v-2a1 1 0 0 0-1-1h-6a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2"/></svg>';
        var CANVAS_ICON_CHECK = '<svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5l3.2 3.2L13 4.8"/></svg>';
        Array.prototype.slice.call(canvasRoot.querySelectorAll('pre')).forEach(function (pre, n) {
          var box = document.createElement('div');
          box.className = '__cv-code';
          var head = document.createElement('div');
          head.className = '__cv-head';
          var label = document.createElement('span');
          var code = pre.querySelector('code');
          var m = code && /(?:^|\\s)language-(\\S+)/.exec(code.className);
          label.textContent = m ? m[1] : '';
          var copy = document.createElement('button');
          copy.type = 'button';
          copy.innerHTML = CANVAS_ICON_COPY + 'Copy';
          var copyTimer = null;
          copy.addEventListener('click', function () {
            parent.postMessage({ type: 'canvas-copy-code', index: n }, '*');
            copy.innerHTML = CANVAS_ICON_CHECK + 'Copied';
            copy.className = '__cv-done';
            clearTimeout(copyTimer);
            copyTimer = setTimeout(function () {
              copy.innerHTML = CANVAS_ICON_COPY + 'Copy';
              copy.className = '';
            }, 1400);
          });
          head.appendChild(label);
          head.appendChild(copy);
          pre.parentNode.insertBefore(box, pre);
          box.appendChild(head);
          box.appendChild(pre);
          var lines = pre.textContent.replace(/\\n$/, '').split('\\n').length;
          if (lines > CANVAS_CLAMP_LINES) {
            box.className += ' __cv-clamped';
            var more = document.createElement('button');
            more.type = 'button';
            more.className = '__cv-more';
            more.textContent = 'Show all ' + lines + ' lines';
            more.addEventListener('click', function () {
              var clamped = box.classList.toggle('__cv-clamped');
              more.textContent = clamped ? 'Show all ' + lines + ' lines' : 'Collapse';
              canvasSendHeight();
            });
            box.appendChild(more);
          }
        });

        // The block is dark and its text light, but a post can paint its own
        // background on a span inside it (a diff's pink and green rows) and
        // leave the text colour to inherit, giving light text on a pale row.
        // Walk each <pre>'s elements in order, blend every background over
        // its parent's, and where the text falls below 4.5:1 against the
        // result, set whichever of near-black or near-white reads better.
        var CANVAS_PRE_BG = [30, 31, 36, 1];
        function canvasRgba(s) {
          var m = /rgba?\\(([^)]+)\\)/.exec(s);
          if (!m) return [0, 0, 0, 0];
          var p = m[1].split(/[ ,\\/]+/).filter(Boolean).map(parseFloat);
          return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1];
        }
        function canvasBlend(top, under) {
          var a = top[3];
          return [0, 1, 2].map(function (i) { return top[i] * a + under[i] * (1 - a); }).concat([1]);
        }
        function canvasLum(c) {
          var l = c.slice(0, 3).map(function (v) {
            v /= 255;
            return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
          });
          return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2];
        }
        function canvasContrast(a, b) {
          var x = canvasLum(a), y = canvasLum(b);
          return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
        }
        canvasRoot.querySelectorAll('pre').forEach(function (pre) {
          var bgOf = new Map();
          bgOf.set(pre, CANVAS_PRE_BG);
          pre.querySelectorAll('*').forEach(function (el) {
            var cs = getComputedStyle(el);
            var bg = canvasBlend(canvasRgba(cs.backgroundColor), bgOf.get(el.parentElement));
            bgOf.set(el, bg);
            var fg = canvasRgba(cs.color);
            if (fg[3] < 1) fg = canvasBlend(fg, bg);
            if (canvasContrast(fg, bg) >= 4.5) return;
            var dark = [26, 26, 26, 1], light = [230, 230, 230, 1];
            var pick = canvasContrast(dark, bg) >= canvasContrast(light, bg) ? '#1a1a1a' : '#e6e6e6';
            el.style.setProperty('color', pick, 'important');
          });
        });

        // The rest of the post. Its own colours stay, but a post written
        // for the other theme (dark text on a dark surface, or pale text on
        // a pale panel) gets the same 4.5:1 fix. Each element's background
        // is blended over its parent's, starting from the page surface; one
        // with a background image or gradient has no known colour, so it
        // passes its parent's through and is left alone.
        var CANVAS_SURFACE = canvasRgba(getComputedStyle(document.body).backgroundColor);
        var cvBg = new Map();
        cvBg.set(canvasRoot, CANVAS_SURFACE);
        canvasRoot.querySelectorAll('*').forEach(function (el) {
          var parentBg = cvBg.get(el.parentElement) || CANVAS_SURFACE;
          if (el.closest('.__cv-code') || el.closest('svg')) {
            cvBg.set(el, parentBg);
            return;
          }
          var cs = getComputedStyle(el);
          var unknown = cs.backgroundImage !== 'none';
          var bg = unknown ? parentBg : canvasBlend(canvasRgba(cs.backgroundColor), parentBg);
          cvBg.set(el, bg);
          if (unknown) return;
          var hasText = Array.prototype.some.call(el.childNodes, function (n) {
            return n.nodeType === 3 && n.nodeValue.trim() !== '';
          });
          if (!hasText) return;
          var fg = canvasRgba(cs.color);
          if (fg[3] < 1) fg = canvasBlend(fg, bg);
          if (canvasContrast(fg, bg) >= 4.5) return;
          var dark = [26, 26, 26, 1], light = [232, 230, 225, 1];
          var pick = canvasContrast(dark, bg) >= canvasContrast(light, bg) ? '#1a1a1a' : '#e8e6e1';
          el.style.setProperty('color', pick, 'important');
        });
      </script>
    `;
    // Injected after the post's markup so it wins at equal specificity.
    // The page colours are the active theme's surface and ink; only html,
    // body and the root wrapper are forced, and the contrast pass in the
    // script above fixes any text a post left unreadable against them.
    const postStyle = `<style>
      html{color-scheme:${theme}!important;}
      html,body,#__canvas_root{background:${surface}!important;background-color:${surface}!important;color:${ink}!important;}
      .__cv-code{margin:4px 0 12px;border-radius:8px;background:#1e1f24;color:#e6e6e6;overflow:hidden;}
      .__cv-head{display:flex;align-items:center;justify-content:space-between;height:30px;padding:0 4px 0 12px;font:12px ui-monospace,Menlo,Consolas,monospace;color:#9a9ca5;border-bottom:1px solid rgba(255,255,255,0.07);}
      .__cv-head button{border:0;background:none;color:#9a9ca5;height:24px;padding:0 8px;border-radius:5px;display:inline-flex;align-items:center;gap:5px;font:12px -apple-system,sans-serif;cursor:pointer;}
      .__cv-head button:hover{background:rgba(255,255,255,0.08);color:#e6e6e6;}
      .__cv-head button.__cv-done{color:#86efac;}
      .__cv-code pre{margin:0;padding:10px 14px 12px;border:0;border-radius:0;box-sizing:border-box;background:#1e1f24;color:#e6e6e6;white-space:pre-wrap;overflow-wrap:anywhere;font:12.5px/1.55 ui-monospace,Menlo,Consolas,monospace;tab-size:2;}
      .__cv-code pre code{background:none;padding:0;border:0;color:inherit;font:inherit;}
      .__cv-code.__cv-clamped pre{max-height:calc(14 * 1.55em + 22px);overflow-y:hidden;-webkit-mask-image:linear-gradient(#000 70%,transparent);mask-image:linear-gradient(#000 70%,transparent);}
      .__cv-more{display:block;width:100%;border:0;border-top:1px solid rgba(255,255,255,0.07);background:none;color:#9a9ca5;padding:6px;cursor:pointer;font:12px -apple-system,sans-serif;}
      .__cv-more:hover{color:#e6e6e6;}
      a[href^="#canvas-open-"]:focus-visible{outline:2px solid #2563eb;outline-offset:2px;border-radius:3px;}
      .__cv-linkbar{position:fixed;z-index:10;display:flex;gap:2px;padding:3px;background:#1a1a1a;border-radius:7px;box-shadow:0 6px 16px rgba(0,0,0,0.2);}
      .__cv-linkbar button{border:0;background:none;color:#fff;height:26px;padding:0 8px;border-radius:5px;font:12px -apple-system,sans-serif;white-space:nowrap;cursor:pointer;}
      .__cv-linkbar button:hover,.__cv-linkbar button:focus-visible{background:rgba(255,255,255,0.14);}
    </style>`;
    return (
      `<!doctype html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
      `<script>(function(){var t='${theme}',m=window.matchMedia.bind(window);` +
      `window.matchMedia=function(q){var r=/prefers-color-scheme\\s*:\\s*(dark|light)/i.exec(String(q));` +
      `if(!r)return m(q);var on=r[1].toLowerCase()===t;` +
      `return{matches:on,media:String(q),onchange:null,addEventListener:function(){},removeEventListener:function(){},` +
      `addListener:function(){},removeListener:function(){},dispatchEvent:function(){return false;}};};})();</script>` +
      `<style>html,body{margin:0;overflow:hidden;}` +
      `body{font-family:-apple-system,sans-serif;}` +
      `img[src*="/api/cards/"]:not(a img){cursor:zoom-in;}` +
      `#__canvas_root{overflow:hidden;box-sizing:border-box;overflow-wrap:anywhere;}` +
      `:where(#__canvas_root) *{max-width:100%;}` +
      `:where(#__canvas_root) :is(img,video){height:auto;}` +
      `mark[data-canvas-hit]{background:#fde68a;color:inherit;border-radius:2px;}</style>` +
      `</head><body><div id="__canvas_root">${html}</div>${postStyle}${resizeScript}</body></html>`
    );
  }

  // Finds the card whose iframe's contentWindow is `source` — the only way
  // a card is identified from a postMessage, since the message itself never
  // carries a card id (an untrusted iframe naming its own card id would let
  // one card's script open another card's targets).
  function cardFrames() {
    const frames = Array.from(cardsEl.querySelectorAll("iframe"));
    if (sheet) frames.push(...sheet.dialog.querySelectorAll("iframe"));
    return frames;
  }

  function cardForFrameSource(source) {
    for (const frame of cardFrames()) {
      if (frame.contentWindow === source) {
        const el = frame.closest(".card");
        return el ? cards.get(el.dataset.cardId) : null;
      }
    }
    return null;
  }

  // card.targets[index] for a message from a card iframe, or null when the
  // sender is not one or the index names no target.
  function targetForMessage(source, index) {
    const card = cardForFrameSource(source);
    if (!card) return null;
    if (!Number.isInteger(index) || index < 0 || index >= card.targets.length) {
      return null;
    }
    return card.targets[index];
  }

  // A path under the home directory reads as ~/…; macOS homes are /Users/<name>.
  function displayTarget(target) {
    return target.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
  }

  // The iframe currently showing a link bar; the stream's scroll hides it.
  let linkbarSource = null;

  // The frame cannot always see the pointer leave it, so the parent does:
  // a pointer over anything but that frame hides its bar.
  document.addEventListener("mouseover", (event) => {
    if (linkbarSource && event.target.contentWindow !== linkbarSource) {
      hideLinkbar();
    }
  });

  function hideLinkbar() {
    if (!linkbarSource) return;
    linkbarSource.postMessage({ type: "canvas-hide-linkbar" }, "*");
    linkbarSource = null;
  }

  // `canvas data` pushes a value into a card's running script as
  // `{type: 'canvas-data', value}`. The latest value is also replayed once an
  // iframe loads, because an update or a theme change builds a new one.
  function postCardData(frame, value) {
    if (frame && frame.contentWindow) {
      frame.contentWindow.postMessage({ type: "canvas-data", value }, "*");
    }
  }

  // How many values each card has been pushed live. A replay that started
  // before a push finished after it would overwrite the newer value with the
  // older stored one, so a replay posts only if no push arrived meanwhile.
  const cardDataPushes = new Map();

  function replayCardData(cardId, frame) {
    const pushes = cardDataPushes.get(cardId) || 0;
    fetch(`/api/cards/${cardId}/data`)
      .then((response) => (response.ok ? response.json() : undefined))
      .then((value) => {
        if (value !== undefined && (cardDataPushes.get(cardId) || 0) === pushes) {
          postCardData(frame, value);
        }
      })
      .catch(() => {});
  }

  function deliverCardData(cardId, value) {
    cardDataPushes.set(cardId, (cardDataPushes.get(cardId) || 0) + 1);
    // The card in the feed, its widget and its open sheet all carry the id.
    for (const frame of document.querySelectorAll(
      `[data-card-id="${CSS.escape(cardId)}"] iframe`
    )) {
      postCardData(frame, value);
    }
  }

  function postTargetKinds(frame, card) {
    if (!frame.contentWindow) return;
    const kinds = card.targets.map((t) => (/^https?:\/\//.test(t) ? "url" : "path"));
    frame.contentWindow.postMessage({ type: "canvas-target-kinds", kinds }, "*");
  }

  window.addEventListener("message", (event) => {
    const data = event.data;
    if (!data) return;

    if (data.type === "canvas-resize") {
      for (const frame of cardFrames()) {
        if (frame.contentWindow === event.source) {
          const cardEl = frame.closest(".card");
          // A hidden card's iframe reports height 0; keep its last real
          // height so showing it again doesn't shift every card below.
          if (cardEl.hidden) break;
          const wasAbove = cardIsAboveViewport(cardEl);
          const before = frame.offsetHeight;
          frame.style.height = `${Math.max(20, data.height)}px`;
          // An arriving card clips at --arrive-height, measured before this
          // iframe reported its real height; keep the clip at the content's
          // height so the post is never cut off mid-slide.
          if (cardEl.classList.contains("arriving")) {
            cardEl.style.setProperty("--arrive-height", `${cardEl.scrollHeight}px`);
          }
          if (wasAbove) holdPlace(frame.offsetHeight - before);
          break;
        }
      }
      return;
    }

    if (data.type === "canvas-open" || data.type === "canvas-copy-target") {
      // Only a real card iframe's contentWindow may name a target — never
      // the top window itself, and never an index a card iframe cannot
      // name a path for.
      const target = targetForMessage(event.source, data.index);
      if (target === null) return;
      if (data.type === "canvas-copy-target") {
        navigator.clipboard
          .writeText(target)
          .then(() => toast(`Copied ${displayTarget(target)}`))
          .catch(() => toast("Couldn't copy"));
        return;
      }
      fetch("/api/open", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ path: target }),
      }).catch(() => {});
      return;
    }

    if (data.type === "canvas-linkbar") {
      const card = cardForFrameSource(event.source);
      if (!card) return;
      linkbarSource = data.open === true ? event.source : null;
      return;
    }

    if (data.type === "canvas-copy-code") {
      // Index-only, like canvas-open: the text comes from the parent's own
      // parse of card.html, never from the message.
      const card = cardForFrameSource(event.source);
      if (!card) return;
      const index = data.index;
      const doc = new DOMParser().parseFromString(card.html, "text/html");
      // <noscript> content is live markup in this inert parse but plain text
      // in the iframe, so its <pre> elements are not counted.
      const pres = Array.from(doc.querySelectorAll("pre")).filter(
        (pre) => !pre.closest("noscript")
      );
      if (!Number.isInteger(index) || index < 0 || index >= pres.length) {
        return;
      }
      navigator.clipboard
        .writeText(pres[index].textContent)
        .catch(() => toast("Couldn't copy code"));
      return;
    }

    if (data.type === "canvas-reply") {
      // canvas-17z: the card is identified from event.source, same rule as
      // canvas-open — an untrusted iframe never names its own id.
      const card = cardForFrameSource(event.source);
      if (!card) return;
      fetch(`/api/cards/${card.id}/reply`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(data.value),
      }).catch(() => {});
      return;
    }

    if (data.type === "canvas-image") {
      // Same rule as canvas-open: the card comes from the sending frame,
      // never from the message, and the index must name one of its images.
      const card = cardForFrameSource(event.source);
      if (!card) return;
      const index = data.index;
      if (!Number.isInteger(index) || index < 0 || index >= card.images.length) {
        return;
      }
      openLightbox(card, index);
    }
  });

  // Pinned posts. A card holding a pin leaves the feed for the shelf under the
  // toolbar: one small widget per pin (the pin's own widgetHtml, else the
  // card's first heading) in slot order. A widget opens its full card as a
  // sheet over the dimmed feed. The shelf element exists only while something
  // is pinned. A widget is replaced one at a time and never moved, because
  // moving an iframe in the document reloads it.
  const WIDGET_W = 200;
  const WIDGET_GAP = 10;
  let shelf = null; // { root, label, more, row } while any card is pinned
  let shelfExpanded = false;
  let sheet = null; // { cardId, scrim, dialog } while a sheet is open

  function pinOrder(a, b) {
    if (a.pin.slot !== b.pin.slot) return a.pin.slot < b.pin.slot ? -1 : 1;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  }

  function pinnedCards() {
    return Array.from(cards.values()).filter((c) => c.pin).sort(pinOrder);
  }

  function widgetFor(cardId) {
    return shelf
      ? shelf.row.querySelector(`.widget[data-card-id="${CSS.escape(cardId)}"]`)
      : null;
  }

  function shortAge(iso) {
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return "";
    const s = Math.max(0, Math.floor((Date.now() - then) / 1000));
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m`;
    if (s < 86400) return `${Math.floor(s / 3600)}h`;
    return `${Math.floor(s / 86400)}d`;
  }

  // The tile a pin with no widgetHtml shows: the post's first heading, else
  // the first line of its text, else its slot.
  function cardTitle(card) {
    const doc = new DOMParser().parseFromString(card.html, "text/html");
    const heading = doc.querySelector("h1, h2, h3, h4, h5, h6");
    const text = heading ? heading.textContent.trim() : "";
    if (text) return text;
    const line = postText(card).split("\n").map((l) => l.trim()).find(Boolean);
    return line ? line.slice(0, 120) : card.pin.slot;
  }

  function buildWidget(card) {
    const el = document.createElement("div");
    el.className = "widget";
    el.dataset.cardId = card.id;
    el.dataset.sessionId = card.sessionId;
    const session = sessions.get(card.sessionId);
    if (session) el.style.setProperty("--session-colour", sessionColour(session));
    const title = cardTitle(card);

    if (card.pin.widgetHtml) {
      const frame = document.createElement("iframe");
      frame.setAttribute("sandbox", "allow-scripts");
      frame.tabIndex = -1;
      frame.setAttribute("aria-hidden", "true");
      frame.srcdoc = buildIframeDoc(card.pin.widgetHtml);
      frame.addEventListener("load", () => replayCardData(card.id, frame));
      el.appendChild(frame);
    } else {
      const tile = document.createElement("div");
      tile.className = "w-title";
      tile.textContent = title;
      el.appendChild(tile);
    }

    const meta = document.createElement("span");
    meta.className = "w-meta";
    const dot = document.createElement("span");
    dot.className = "w-dot";
    const age = document.createElement("span");
    age.className = "w-age";
    age.textContent = shortAge(card.at);
    meta.append(dot, age);
    el.appendChild(meta);

    const error = card.pin.refreshError;
    if (error) {
      const mark = document.createElement("span");
      mark.className = "w-error";
      mark.textContent = "!";
      el.appendChild(mark);
    }

    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("aria-label", error ? `Open ${title} (refresh failed)` : `Open ${title}`);
    button.setAttribute("aria-expanded", "false");
    if (error) button.title = error;
    button.addEventListener("click", () => openSheet(card.id));
    el.appendChild(button);
    return el;
  }

  function ensureShelf() {
    if (shelf) return;
    const root = document.createElement("section");
    root.className = "shelf";
    root.setAttribute("aria-label", "Pinned posts");
    const head = document.createElement("div");
    head.className = "shelf-head";
    const label = document.createElement("div");
    label.className = "shelf-label";
    const labelText = document.createElement("span");
    label.append(buildIcon("pin"), labelText);
    const more = document.createElement("button");
    more.type = "button";
    more.className = "shelf-more";
    more.addEventListener("click", () => {
      shelfExpanded = !shelfExpanded;
      measureShelf();
    });
    head.append(label, more);
    const row = document.createElement("div");
    row.className = "shelf-row";
    root.append(head, row);
    bodySplitEl.before(root);
    shelf = { root, label: labelText, more, row };
    new ResizeObserver(measureShelf).observe(row);
  }

  // How many widgets fit the row is arithmetic on their fixed width. Those
  // past it stay out of the tab order until "+N" wraps the row to show them.
  function measureShelf() {
    if (!shelf) return;
    const widgets = Array.from(shelf.row.children);
    const capacity = Math.max(
      1,
      Math.floor((shelf.row.clientWidth - 6 + WIDGET_GAP) / (WIDGET_W + WIDGET_GAP))
    );
    const hidden = Math.max(0, widgets.length - capacity);
    if (hidden === 0) shelfExpanded = false;
    // Focus or a drag can scroll the clipped row; it always starts at the left.
    shelf.row.scrollLeft = 0;
    const collapsed = hidden > 0 && !shelfExpanded;
    shelf.row.classList.toggle("overflowing", collapsed);
    shelf.row.classList.toggle("expanded", hidden > 0 && shelfExpanded);
    widgets.forEach((w, i) => {
      w.inert = collapsed && i >= capacity;
    });
    shelf.more.hidden = hidden === 0;
    shelf.more.textContent = shelfExpanded ? "Show less" : `+${hidden}`;
    shelf.more.setAttribute("aria-expanded", String(shelfExpanded));
  }

  // The label, the overflow and the shelf's own existence follow the pins.
  function updateShelf() {
    const n = pinnedCards().length;
    if (n === 0) {
      if (shelf) shelf.root.remove();
      shelf = null;
      shelfExpanded = false;
      return;
    }
    ensureShelf();
    shelf.label.textContent = `Pinned · ${n}`;
    measureShelf();
  }

  function markWidgetOpen() {
    if (!shelf) return;
    for (const w of shelf.row.children) {
      const open = Boolean(sheet) && w.dataset.cardId === sheet.cardId;
      w.classList.toggle("open", open);
      w.querySelector("button").setAttribute("aria-expanded", String(open));
    }
  }

  function upsertWidget(card) {
    ensureShelf();
    const old = widgetFor(card.id);
    if (old) old.remove();
    const el = buildWidget(card);
    const next = Array.from(shelf.row.children).find((w) => pinOrder(card, cards.get(w.dataset.cardId)) < 0);
    shelf.row.insertBefore(el, next || null);
    updateShelf();
    if (sheet && sheet.cardId === card.id) {
      paintSheet();
      markWidgetOpen();
    }
  }

  function removeWidget(cardId) {
    if (sheet && sheet.cardId === cardId) closeSheet({ restoreFocus: false });
    const el = widgetFor(cardId);
    if (!el) return;
    el.remove();
    updateShelf();
  }

  // Rebuilds every widget: the initial load, a reconnect, a theme change.
  function rebuildShelf() {
    if (shelf) shelf.row.replaceChildren();
    const pins = pinnedCards();
    if (pins.length) {
      ensureShelf();
      shelf.row.append(...pins.map(buildWidget));
    }
    updateShelf();
    if (sheet) {
      const card = cards.get(sheet.cardId);
      if (card && card.pin) {
        paintSheet();
        markWidgetOpen();
      } else {
        closeSheet({ restoreFocus: false });
      }
    }
  }

  function paintSheet() {
    const card = cards.get(sheet.cardId);
    const el = renderCard(card);
    const header = el.querySelector(".card-header");
    const close = document.createElement("button");
    close.type = "button";
    close.className = "icon-btn sheet-close";
    close.setAttribute("aria-label", "Close");
    close.appendChild(buildIcon("x"));
    close.addEventListener("click", () => closeSheet());
    header.appendChild(close);
    const error = card.pin.refreshError;
    if (error) {
      const line = document.createElement("div");
      line.className = "sheet-error";
      line.setAttribute("role", "alert");
      line.textContent = error;
      el.insertBefore(line, el.querySelector(".card-body"));
    }
    sheet.dialog.setAttribute("aria-label", cardTitle(card));
    // A refresh rebuilds the card under a reader: keep their place and focus.
    const scrollTop = sheet.dialog.scrollTop;
    const hadFocus = sheet.dialog.contains(document.activeElement);
    sheet.dialog.replaceChildren(el);
    sheet.dialog.scrollTop = scrollTop;
    if (hadFocus) close.focus();
    closeMenuIfDetached();
  }

  function openSheet(cardId) {
    const card = cards.get(cardId);
    if (!card || !card.pin) return;
    if (sheet && sheet.cardId === cardId) return;
    if (sheet) closeSheet({ restoreFocus: false });
    const scrim = document.createElement("div");
    scrim.className = "sheet-scrim";
    scrim.addEventListener("click", (e) => {
      if (e.target === scrim) closeSheet();
    });
    const dialog = document.createElement("div");
    dialog.className = "sheet";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    scrim.appendChild(dialog);
    sheet = { cardId, scrim, dialog };
    mainColEl.appendChild(scrim);
    streamEl.inert = true;
    paintSheet();
    markWidgetOpen();
    dialog.querySelector(".sheet-close").focus();
  }

  function closeSheet({ restoreFocus = true } = {}) {
    if (!sheet) return;
    const { cardId, scrim } = sheet;
    sheet = null;
    scrim.remove();
    streamEl.inert = false;
    closeMenuIfDetached();
    markWidgetOpen();
    if (restoreFocus) {
      const widget = widgetFor(cardId);
      if (widget) widget.querySelector("button").focus();
    }
  }

  // Ahead of the menu's and the lightbox's own Escape handling: a menu or the
  // lightbox opened over the sheet closes first, and the sheet on the next press.
  window.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || !sheet || openMenu || lightbox) return;
      e.stopPropagation();
      closeSheet();
    },
    true
  );

  setInterval(() => {
    if (!shelf) return;
    for (const w of shelf.row.children) {
      const card = cards.get(w.dataset.cardId);
      if (card) w.querySelector(".w-age").textContent = shortAge(card.at);
    }
  }, 1000);

  function renderCard(card) {
    const el = document.createElement("div");
    el.className = "card";
    el.dataset.cardId = card.id;
    el.dataset.sessionId = card.sessionId;
    el.dataset.at = card.at;
    const session = sessions.get(card.sessionId);
    if (session) el.style.setProperty("--session-colour", sessionColour(session));

    const header = document.createElement("div");
    header.className = "card-header";
    header.appendChild(buildAgentTile());
    const headerInfo = document.createElement("div");
    headerInfo.className = "card-header-info";
    const nameSpan = document.createElement("span");
    nameSpan.className = "session-name";
    nameSpan.textContent = sessionName(card.sessionId);
    const repoSpan = document.createElement("span");
    repoSpan.className = "session-repo";
    repoSpan.textContent = sessionRepo(card.sessionId);
    repoSpan.hidden = !repoSpan.textContent;
    const sep = document.createTextNode(" · ");
    const timeSpan = document.createElement("span");
    timeSpan.className = "time";
    timeSpan.textContent = relativeTime(card.at);
    headerInfo.appendChild(nameSpan);
    headerInfo.appendChild(repoSpan);
    headerInfo.appendChild(sep);
    headerInfo.appendChild(timeSpan);
    header.appendChild(headerInfo);

    const moreBtn = document.createElement("button");
    moreBtn.type = "button";
    moreBtn.className = "icon-btn card-more-btn";
    moreBtn.setAttribute("aria-label", "Post actions");
    moreBtn.setAttribute("aria-haspopup", "menu");
    moreBtn.setAttribute("aria-expanded", "false");
    moreBtn.appendChild(buildIcon("more"));
    moreBtn.addEventListener("click", () => toggleCardMenu(card, header, moreBtn));
    header.appendChild(moreBtn);
    el.appendChild(header);

    const body = document.createElement("div");
    body.className = "card-body";

    const iframe = document.createElement("iframe");
    iframe.setAttribute("sandbox", "allow-scripts");
    iframe.srcdoc = buildIframeDoc(card.html);
    iframe.addEventListener("load", () => {
      postTargetKinds(iframe, card);
      if (highlightQuery) postHighlight(iframe);
      replayCardData(card.id, iframe);
    });
    body.appendChild(iframe);

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
    closeMenu();
    const sorted = sortedCards();
    cardsEl.innerHTML = "";
    for (const card of sorted) {
      if (!card.pin) cardsEl.appendChild(renderCard(card));
    }
    rebuildShelf();
    refreshVisibility();
  }

  // Insert or replace only the one card that changed, leaving every other
  // card's DOM node (and any live iframe inside it) untouched, so a card
  // updated elsewhere never reloads this card's sandboxed HTML posts.
  function upsertCard(card) {
    const wasPinned = Boolean(cards.get(card.id)?.pin);
    cards.set(card.id, card);
    postTextLower.delete(card.id);

    const existing = cardsEl.querySelector(
      `[data-card-id="${CSS.escape(card.id)}"]`
    );
    if (card.pin) {
      // A pinned card lives on the shelf, not in the feed.
      if (existing) {
        const wasAbove = cardIsAboveViewport(existing);
        const height = existing.offsetHeight + (parseFloat(getComputedStyle(existing).marginBottom) || 0);
        existing.remove();
        if (wasAbove) holdPlace(-height);
        closeMenuIfDetached();
      }
      unseen.cardIds.delete(card.id);
      upsertWidget(card);
      refreshVisibility();
      return;
    }
    removeWidget(card.id);
    // Replacing a card above the reader can change its height (a longer
    // post, a resized image) as surely as a resize or an arrival can — hold
    // the same way, against the height existing had a moment ago.
    const wasAbove = existing ? cardIsAboveViewport(existing) : false;
    const existingHeight = existing
      ? existing.offsetHeight + (parseFloat(getComputedStyle(existing).marginBottom) || 0)
      : 0;
    if (existing) {
      // The replacement is about to detach this card's menu too — close it
      // first (which also clears an armed Delete post inside it), so neither
      // openMenu nor pendingConfirm holds a node no longer in the document.
      existing.remove();
      closeMenuIfDetached();
    }

    const el = renderCard(card);
    el.hidden = !cardMatchesFilter(card);
    const siblings = Array.from(cardsEl.children);
    const insertBefore = siblings.find(
      (child) => new Date(child.dataset.at) < new Date(card.at)
    );
    if (insertBefore) {
      cardsEl.insertBefore(el, insertBefore);
    } else {
      cardsEl.appendChild(el);
    }
    if (wasAbove) {
      const newHeight = el.offsetHeight + (parseFloat(getComputedStyle(el).marginBottom) || 0);
      holdPlace(newHeight - existingHeight);
    }

    // Only a new post from a session the reader can see arrives; replacing
    // a card, or a post from a hidden session, is silent.
    const session = sessions.get(card.sessionId);
    const arrives = !existing && !wasPinned && !(session && isHidden(session));
    if (arrives && !el.hidden) arrive(el, card);

    // A new card changes its session's count — refresh the chip row and
    // drawer without touching any other card, so a card for a
    // non-selected session updates its chip's count and leaves the visible
    // stream alone. A session's first card brings its chip in with motion.
    chipsMayArrive = arrives;
    try {
      refreshVisibility();
    } finally {
      chipsMayArrive = false;
    }

    if (arrives && !el.hidden) pulseChip(card.sessionId);
    if (arrives && el.hidden) {
      toast(`New post from ${sessionName(card.sessionId)} (filtered out)`, "Show", () => {
        setQuery("");
        selectSession(card.sessionId);
        scrollToTop();
      });
    }
  }

  function cardIsAboveViewport(cardEl) {
    if (!cardEl || cardEl.hidden) return false;
    return cardEl.getBoundingClientRect().bottom <= streamEl.getBoundingClientRect().top;
  }

  // Adds a height change above the reader to scrollTop so what they are
  // reading stays where it was.
  function holdPlace(delta) {
    if (!delta || returningToTop || streamEl.scrollTop <= SCROLLED_PX) return;
    streamEl.scrollTop += delta;
  }

  function arrive(el, card) {
    if (streamEl.scrollTop > SCROLLED_PX) {
      const gap = parseFloat(getComputedStyle(el).marginBottom) || 0;
      // A card dated before the reader's position lands below them: nothing
      // to hold and nothing new above.
      if (el.getBoundingClientRect().top >= streamEl.getBoundingClientRect().top) return;
      if (!returningToTop) streamEl.scrollTop += el.offsetHeight + gap;
      unseen.cardIds.add(card.id);
    } else if (animating()) {
      el.style.setProperty("--arrive-height", `${el.offsetHeight}px`);
      el.classList.add("arriving");
    }
    if (animating()) el.classList.add("ring");
    // Each class comes off when its own animation ends; the timer covers a
    // card the filter hides mid-animation, which never fires animationend.
    el.addEventListener("animationend", (e) => {
      if (e.animationName === "card-slide") el.classList.remove("arriving");
      if (e.animationName === "card-ring") el.classList.remove("ring");
    });
    setTimeout(() => el.classList.remove("arriving", "ring"), 1900);
  }

  function pulseChip(sessionId) {
    if (!animating()) return;
    const chip = chipsEl.querySelector(`.chip[data-session-id="${CSS.escape(sessionId)}"]`);
    if (!chip || chip.hasAttribute("data-arriving")) return;
    chip.classList.add("pulse");
    chip.addEventListener("animationend", () => chip.classList.remove("pulse"), { once: true });
    setTimeout(() => chip.classList.remove("pulse"), 1900);
  }

  function scrollToTop() {
    returningToTop = streamEl.scrollTop > SCROLLED_PX;
    streamEl.scrollTo({ top: 0, behavior: reducedMotion.matches ? "auto" : "smooth" });
    clearUnseen();
  }

  function clearUnseen() {
    unseen.cardIds.clear();
    renderPill();
  }

  // Only the unseen cards the current chip filter and search still show —
  // recomputed from cards.get() each render, so switching filters after
  // arrival can't leave the pill describing posts no longer in view.
  function unseenVisibleCards() {
    const result = [];
    for (const id of unseen.cardIds) {
      const card = cards.get(id);
      if (card && cardMatchesFilter(card)) result.push(card);
    }
    return result;
  }

  // "↑", up to four dots in the colours of the sessions with unseen posts,
  // and the count.
  function renderPill() {
    const visible = unseenVisibleCards();
    newPillEl.hidden = visible.length === 0;
    newPillEl.dataset.count = String(visible.length);
    if (visible.length === 0) return;
    const dots = document.createElement("span");
    dots.className = "pill-dots";
    const sessionIds = Array.from(new Set(visible.map((c) => c.sessionId))).slice(-4);
    for (const id of sessionIds) {
      const s = sessions.get(id);
      if (!s) continue;
      const dot = document.createElement("span");
      dot.className = "session-dot";
      dot.style.setProperty("--session-colour", sessionColour(s));
      dots.appendChild(dot);
    }
    const n = visible.length;
    newPillEl.replaceChildren("↑", dots, `${n} new post${n === 1 ? "" : "s"}`);
  }

  newPillEl.addEventListener("click", scrollToTop);
  toTopEl.appendChild(buildIcon("arrow-up"));
  toTopEl.addEventListener("click", scrollToTop);

  streamEl.addEventListener("scroll", () => {
    hideLinkbar();
    const y = streamEl.scrollTop;
    toTopEl.hidden = y <= TO_TOP_PX;
    if (y < SCROLLED_PX) {
      returningToTop = false;
      if (unseen.cardIds.size > 0) clearUnseen();
    }
  });
  // The reader taking the wheel or a finger back ends a smooth scroll to top.
  for (const type of ["wheel", "touchstart"]) {
    streamEl.addEventListener(type, () => (returningToTop = false), { passive: true });
  }

  // A session's name and repo can change (e.g. re-registration); patch just
  // the header text and colour of that session's existing cards instead of
  // rebuilding them. A new repo is a new colour key.
  function patchSessionNames(sessionId) {
    const name = sessionName(sessionId);
    const repo = sessionRepo(sessionId);
    const card = `.card[data-session-id="${CSS.escape(sessionId)}"]`;
    const colour = sessionColour(sessions.get(sessionId));
    for (const el of document.querySelectorAll(`${card}, .widget[data-session-id="${CSS.escape(sessionId)}"]`)) {
      el.style.setProperty("--session-colour", colour);
    }
    for (const el of cardsEl.querySelectorAll(`${card} .session-name`)) {
      el.textContent = name;
    }
    for (const el of cardsEl.querySelectorAll(`${card} .session-repo`)) {
      el.textContent = repo;
      el.hidden = !repo;
    }
  }

  // The lightbox shows one of a card's images full size; with more than one,
  // the arrow buttons and keys step through that card's images in post order.
  let lightbox = null; // { card, index } while open

  // Zoom is relative to the fitted image: scale 1 is "fit", and tx/ty move the
  // image from the window centre (the image's transform-origin). Pan is only
  // allowed while the scaled image is larger than the window, and stops at its
  // edges.
  const ZOOM_MAX = 8;
  const ZOOM_CLICK = 2.5;
  const ZOOM_KEY_STEP = 1.25;
  const DRAG_SLOP = 4; // px of pointer travel before a press counts as a drag
  const zoom = { scale: 1, tx: 0, ty: 0 };
  let imageDrag = null; // { id, x, y, tx, ty, moved } while a press is down on the image
  let backdropClickSuppressed = false;

  function paintZoom() {
    const { scale } = zoom;
    const maxX = Math.max(0, (overlayImgEl.offsetWidth * scale - overlayEl.clientWidth) / 2);
    const maxY = Math.max(0, (overlayImgEl.offsetHeight * scale - overlayEl.clientHeight) / 2);
    zoom.tx = Math.min(maxX, Math.max(-maxX, zoom.tx));
    zoom.ty = Math.min(maxY, Math.max(-maxY, zoom.ty));
    overlayImgEl.style.transform =
      scale === 1 ? "" : `translate(${zoom.tx}px, ${zoom.ty}px) scale(${scale})`;
    overlayImgEl.classList.toggle("zoomed", scale > 1);
    overlayZoomEl.hidden = scale === 1;
    if (scale > 1 && overlayImgEl.naturalWidth) {
      const pct = (scale * overlayImgEl.offsetWidth) / overlayImgEl.naturalWidth;
      overlayZoomEl.textContent = `${Math.round(pct * 100)}%`;
    }
  }

  // Zoom to `next`, keeping the image point under (clientX, clientY) still;
  // with no point, the window centre.
  function zoomTo(next, clientX, clientY) {
    next = Math.min(ZOOM_MAX, Math.max(1, next));
    const rect = overlayEl.getBoundingClientRect();
    const dx = (clientX ?? rect.left + rect.width / 2) - (rect.left + rect.width / 2);
    const dy = (clientY ?? rect.top + rect.height / 2) - (rect.top + rect.height / 2);
    const ratio = next / zoom.scale;
    zoom.tx = dx - (dx - zoom.tx) * ratio;
    zoom.ty = dy - (dy - zoom.ty) * ratio;
    zoom.scale = next;
    paintZoom();
  }

  function resetZoom() {
    zoom.scale = 1;
    zoom.tx = 0;
    zoom.ty = 0;
    paintZoom();
  }

  function paintLightbox() {
    const { card, index } = lightbox;
    const count = card.images.length;
    resetZoom();
    overlayImgEl.src = `/api/cards/${encodeURIComponent(card.id)}/images/${index}`;
    overlayPrevEl.hidden = count < 2;
    overlayNextEl.hidden = count < 2;
    overlayCountEl.hidden = count < 2;
    overlayCountEl.textContent = `${index + 1} of ${count}`;
  }

  function openLightbox(card, index) {
    lightbox = { card, index };
    paintLightbox();
    overlayEl.hidden = false;
    // The click that opened it happened inside the card's iframe, which
    // still holds keyboard focus — pull it out here, or Escape and the arrow
    // keys go to the post instead of this window's keydown handler.
    overlayEl.focus();
  }

  function closeLightbox() {
    lightbox = null;
    imageDrag = null;
    overlayImgEl.classList.remove("dragging");
    overlayEl.hidden = true;
    overlayImgEl.src = "";
    resetZoom();
  }

  function stepLightbox(delta) {
    const count = lightbox.card.images.length;
    lightbox.index = (lightbox.index + delta + count) % count;
    paintLightbox();
  }

  overlayPrevEl.appendChild(buildIcon("chevron-left"));
  overlayNextEl.appendChild(buildIcon("chevron-right"));
  overlayPrevEl.addEventListener("click", (e) => {
    e.stopPropagation();
    stepLightbox(-1);
  });
  overlayNextEl.addEventListener("click", (e) => {
    e.stopPropagation();
    stepLightbox(1);
  });
  overlayCloseEl.appendChild(buildIcon("x"));
  overlayCloseEl.addEventListener("click", (e) => {
    e.stopPropagation();
    closeLightbox();
  });
  overlayEl.addEventListener("pointerdown", () => {
    backdropClickSuppressed = false;
  });
  // Only a click on the bare backdrop closes; one that ends a drag does not.
  overlayEl.addEventListener("click", (e) => {
    if (e.target !== overlayEl) return;
    if (backdropClickSuppressed) {
      backdropClickSuppressed = false;
      return;
    }
    closeLightbox();
  });
  // A press on the image that doesn't travel toggles fit and 2.5x at the
  // click point; one that travels pans when zoomed and does nothing at fit.
  overlayImgEl.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    imageDrag = { id: e.pointerId, x: e.clientX, y: e.clientY, tx: zoom.tx, ty: zoom.ty, moved: false };
    overlayImgEl.setPointerCapture(e.pointerId);
  });
  overlayImgEl.addEventListener("pointermove", (e) => {
    if (!imageDrag || e.pointerId !== imageDrag.id) return;
    const dx = e.clientX - imageDrag.x;
    const dy = e.clientY - imageDrag.y;
    if (!imageDrag.moved && Math.hypot(dx, dy) < DRAG_SLOP) return;
    imageDrag.moved = true;
    if (zoom.scale === 1) return;
    overlayImgEl.classList.add("dragging");
    zoom.tx = imageDrag.tx + dx;
    zoom.ty = imageDrag.ty + dy;
    paintZoom();
  });
  function endImageDrag(e, cancelled) {
    if (!imageDrag || e.pointerId !== imageDrag.id) return;
    const { moved } = imageDrag;
    imageDrag = null;
    overlayImgEl.classList.remove("dragging");
    if (moved) {
      backdropClickSuppressed = true;
    } else if (!cancelled) {
      if (zoom.scale > 1) resetZoom();
      else zoomTo(ZOOM_CLICK, e.clientX, e.clientY);
    }
  }
  overlayImgEl.addEventListener("pointerup", (e) => endImageDrag(e, false));
  overlayImgEl.addEventListener("pointercancel", (e) => endImageDrag(e, true));
  overlayEl.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      // A trackpad pinch arrives as a wheel event with ctrlKey and small deltas.
      const rate = e.ctrlKey ? 0.01 : 0.002;
      // A line-mode wheel reports about 3 per notch where a pixel-mode one reports about 100.
      const delta = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? e.deltaY * 33 : e.deltaY;
      zoomTo(zoom.scale * Math.exp(-delta * rate), e.clientX, e.clientY);
    },
    { passive: false }
  );
  window.addEventListener("resize", () => {
    if (lightbox) paintZoom();
  });
  window.addEventListener("keydown", (e) => {
    if (!lightbox) return;
    if (e.key === "Escape") closeLightbox();
    else if (e.metaKey || e.ctrlKey || e.altKey) return;
    else if (e.key === "+" || e.key === "=") zoomTo(zoom.scale * ZOOM_KEY_STEP);
    else if (e.key === "-") zoomTo(zoom.scale / ZOOM_KEY_STEP);
    else if (e.key === "0") resetZoom();
    else if (e.key === "ArrowLeft" && lightbox.card.images.length > 1) stepLightbox(-1);
    else if (e.key === "ArrowRight" && lightbox.card.images.length > 1) stepLightbox(1);
  });

  async function loadState() {
    const res = await fetch("/api/state");
    const data = await res.json();
    sessions.clear();
    // canvasd lists sessions in no fixed order; oldest first makes "first
    // appearance" — and so each session's colour — the same on every load.
    data.sessions.sort((a, b) => new Date(a.startedAt) - new Date(b.startedAt));
    for (const s of data.sessions) {
      sessions.set(s.id, s);
      sessionColour(s);
    }
    cards.clear();
    postTextLower.clear();
    for (const c of data.cards) cards.set(c.id, c);
    // A card that aged out of the server's window (or was deleted) while
    // this browser was disconnected never passes through removeCard, so
    // drop it here instead of leaving its id in unseen.cardIds forever.
    for (const id of Array.from(unseen.cardIds)) {
      if (!cards.has(id)) unseen.cardIds.delete(id);
    }
    prunePrefs();
    bootstrapRender();
  }

  // canvasd has no TCP port, so there is no EventSource here: Canvas.app holds
  // the one /api/events stream and re-emits it as `canvas-event` (one per
  // server-sent event) and `canvas-stream` ("open" / "closed").
  function connectEvents() {
    const { listen } = window.__TAURI__.event;
    // Listeners are up before the first loadState, so nothing published in
    // between is lost. Every "open" re-fetches the full snapshot: events
    // published while the stream was down (including the up-to-1s gap between
    // this page loading and the app's stream connecting) went to no
    // subscriber and are gone, so the snapshot is how the view catches up.
    const handlers = {};

    const registered = [
      listen("canvas-stream", (event) => {
        if (event.payload === "closed") {
          bannerEl.hidden = false;
        } else {
          bannerEl.hidden = true;
          loadState();
        }
      }),
      listen("canvas-event", (event) => {
        const handle = handlers[event.payload.event];
        if (handle) handle(JSON.parse(event.payload.data));
      }),
    ];

    handlers["session-upserted"] = (session) => {
      const before = sessions.get(session.id);
      sessions.set(session.id, session);
      sessionColour(session);
      refreshVisibility();
      patchSessionNames(session.id);
      // Only the upsert that first sets endedAt on a session this viewer was
      // showing announces the archive; a reload finds it archived silently.
      const endedJustNow = before && !before.endedAt && session.endedAt;
      if (endedJustNow && !isHidden(before) && isHidden(session)) {
        toast(`${session.name} ended — its posts are archived`, "Keep showing", () =>
          showSession(session.id)
        );
      }
    };

    handlers["card-upserted"] = (card) => upsertCard(card);

    handlers["card-data"] = ({ id, value }) => deliverCardData(id, value);

    handlers["card-removed"] = ({ id }) => removeCard(id);

    handlers["session-removed"] = ({ id }) => removeSession(id);

    return Promise.all(registered);
  }

  // A `canvas-post://<card id>` link waits in the app until the viewer takes
  // it. The viewer takes it only once the stream is loaded, so a link that
  // launched the app finds its card; loadState calls this again when done.
  let stateLoaded = false;

  async function openPendingCard() {
    if (!stateLoaded || !tauriCore) return;
    const id = await tauriCore.invoke("take_pending_card");
    if (id) openCardLink(id);
  }

  // Clears whatever hides the card (search, session filter, an archived
  // session), scrolls it to the middle and flashes its ring.
  function openCardLink(id) {
    const card = cards.get(id);
    if (!card) {
      toast("That post is gone — Canvas keeps the newest 500 for 24 hours");
      return;
    }
    setQuery("");
    resetVisibility();
    const session = sessions.get(card.sessionId);
    if (session && isHidden(session)) showSession(session.id);
    if (card.pin) {
      openSheet(id);
      return;
    }
    const el = cardsEl.querySelector(`.card[data-card-id="${CSS.escape(id)}"]`);
    if (!el) return;
    el.scrollIntoView({ block: "center" });
    el.classList.remove("ring");
    void el.offsetWidth;
    el.classList.add("ring");
    setTimeout(() => el.classList.remove("ring"), 1900);
  }

  // Out-of-date banner: shown while the app's bundled `canvas` differs from
  // the installed one (daemon_status.relaunchNeeded). Dismissing hides it for
  // this page's lifetime, i.e. until the next launch; Settings keeps its own
  // Relaunch button.
  const updateBannerEl = document.getElementById("update-banner");
  const updateRelaunchEl = document.getElementById("update-banner-relaunch");
  let updateBannerDismissed = false;

  function pollUpdateBanner() {
    if (updateBannerDismissed) return;
    tauriCore
      .invoke("daemon_status")
      .then((status) => {
        updateBannerEl.hidden = !status.relaunchNeeded;
      })
      .catch(() => {});
  }

  if (tauriCore) {
    updateRelaunchEl.addEventListener("click", () => {
      updateRelaunchEl.disabled = true;
      updateRelaunchEl.textContent = "Relaunching…";
      tauriCore.invoke("relaunch").catch(() => {
        updateRelaunchEl.disabled = false;
        updateRelaunchEl.textContent = "Relaunch";
        toast("Couldn't relaunch");
      });
    });
    document.getElementById("update-banner-dismiss").addEventListener("click", () => {
      updateBannerDismissed = true;
      updateBannerEl.hidden = true;
    });
    pollUpdateBanner();
    setInterval(pollUpdateBanner, 60000);
    window.addEventListener("focus", pollUpdateBanner);
  }

  if (tauriCore) window.__TAURI__.event.listen("canvas-open-card", openPendingCard);

  connectEvents()
    .then(loadState)
    .then(() => {
      stateLoaded = true;
      return openPendingCard();
    });
})();
