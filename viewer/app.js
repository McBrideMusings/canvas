(() => {
  "use strict";

  const sessions = new Map(); // id -> session
  const cards = new Map(); // id -> card
  let selectedSessionId = null; // null = "All"

  const streamEl = document.getElementById("stream");
  const cardsEl = document.getElementById("cards");
  const emptyStateEl = document.getElementById("empty-state");
  const chipsEl = document.getElementById("chips");
  const bannerEl = document.getElementById("disconnected-banner");
  const overlayEl = document.getElementById("image-overlay");
  const overlayImgEl = document.getElementById("image-overlay-img");
  const overlayPrevEl = document.getElementById("image-overlay-prev");
  const overlayNextEl = document.getElementById("image-overlay-next");
  const overlayCountEl = document.getElementById("image-overlay-count");
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
      case "gear":
        svg.appendChild(svgEl("circle", { cx: "12", cy: "12", r: "3" }));
        svg.appendChild(
          svgEl("path", {
            d: "M12 2.8v2.4M12 18.8v2.4M4.2 7.5l2.1 1.2M17.7 15.3l2.1 1.2M4.2 16.5l2.1-1.2M17.7 8.7l2.1-1.2",
          })
        );
        svg.appendChild(svgEl("circle", { cx: "12", cy: "12", r: "7" }));
        break;
      case "eye-off":
        svg.appendChild(svgEl("path", { d: "M3 3l18 18" }));
        svg.appendChild(
          svgEl("path", {
            d: "M10.6 6.2A9.8 9.8 0 0 1 12 6c5 0 9 6 9 6a15 15 0 0 1-2.6 3.2M6.3 7.9C4.2 9.5 3 12 3 12s4 6 9 6a8.6 8.6 0 0 0 3.4-.7",
          })
        );
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
      default:
        throw new Error(`buildIcon: unknown icon name "${name}"`);
    }
    return svg;
  }

  // `text`, when given, follows the icon as the button's visible label (a
  // menu item); icon-only buttons leave it out and rely on aria-label. A
  // text-only button (Settings' Delete) passes a null icon.
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
      shown: ids(stored.shown),
      hidden: ids(stored.hidden),
    };
  }

  function savePrefs() {
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    } catch (e) {}
  }

  const prefs = loadPrefs();

  // Arrival. WKWebView has no CSS scroll anchoring, so a reader scrolled
  // down more than SCROLLED_PX keeps their place by hand: the height of a
  // card inserted above them, and every later height change of a card above
  // them, is added to the stream's scrollTop. `overflow-anchor: none` in the
  // stylesheet stops browsers that do anchor from correcting a second time.
  const SCROLLED_PX = 40;
  const TO_TOP_PX = 500;
  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const unseen = { count: 0, sessionIds: new Set() };
  // True from a click on the pill or back-to-top button until the stream
  // reaches the top, so place-holding does not fight the smooth scroll.
  let returningToTop = false;
  const newPillEl = document.getElementById("new-pill");
  const toTopEl = document.getElementById("to-top");

  function animating() {
    return prefs.animate && !reducedMotion.matches;
  }

  // The one rule for a hidden session. The stream filter, the chip row, the
  // Settings list and the gear's badge all ask it.
  function isHidden(session) {
    if (prefs.hidden.includes(session.id)) return true;
    return !!session.endedAt && prefs.hideEnded && !prefs.shown.includes(session.id);
  }

  function hiddenSessions() {
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
    for (const el of cardsEl.children) {
      const s = sessions.get(el.dataset.sessionId);
      if (s) el.style.setProperty("--session-colour", sessionColour(s));
    }
  }

  // Switching the key hands colours out again from the first palette entry,
  // in the order sessions first appeared. The chips and Settings repaint in
  // the refreshVisibility that follows.
  function setColorBy(value) {
    if (prefs.colorBy === value) return;
    setPrefsAndRefresh(() => {
      prefs.colorBy = value;
      colours = new Map();
      for (const s of sessions.values()) sessionColour(s);
      paintCardColours();
    });
  }

  // A hidden session can't stay selected: its cards would all be filtered
  // out behind a filter the chip row no longer shows.
  function refreshVisibility() {
    const selected = selectedSessionId && sessions.get(selectedSessionId);
    if (selected && isHidden(selected)) selectedSessionId = null;
    renderChips();
    applyFilter();
    renderSettings();
    renderPill();
  }

  function setPrefsAndRefresh(change) {
    change();
    savePrefs();
    refreshVisibility();
  }

  function hideSession(id) {
    const name = sessionName(id);
    setPrefsAndRefresh(() => {
      if (!prefs.hidden.includes(id)) prefs.hidden.push(id);
    });
    toast(`Hid ${name}`, "Undo", () =>
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
      // turning it back on hides all of them again, not a remembered few.
      if (!on) prefs.shown = [];
    });
  }

  // Only Canvas.app's WKWebView injects window.__TAURI__ (withGlobalTauri in
  // tauri.conf.json); a plain browser tab never sees it, which is how the
  // button knows to stay hidden there.
  const pinToggleEl = document.getElementById("pin-toggle");
  const tauriCore = window.__TAURI__ && window.__TAURI__.core;

  pinToggleEl.appendChild(buildIcon("pin"));

  function applyPinnedState(pinned) {
    pinToggleEl.setAttribute("aria-pressed", pinned ? "true" : "false");
    pinToggleEl.setAttribute(
      "aria-label",
      pinned ? "Unpin window" : "Pin window on top"
    );
    setButtonIcon(pinToggleEl, pinned ? "pin-fill" : "pin");
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

  // `owner/repo` on GitHub, or "" when the session's directory has none.
  function sessionRepo(sessionId) {
    const s = sessions.get(sessionId);
    return (s && s.repo) || "";
  }

  function sessionCardCount(sessionId) {
    let n = 0;
    for (const c of cards.values()) {
      if (c.sessionId === sessionId) n++;
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

  function selectSession(id) {
    selectedSessionId = id;
    renderChips();
    applyFilter();
    const pressed = chipsEl.querySelector('.chip[aria-pressed="true"]');
    if (pressed) pressed.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  // Visible sessions, most recent post first; a session with no posts
  // counts from when it started.
  function chipSessions() {
    const latest = new Map();
    for (const c of cards.values()) {
      const at = new Date(c.at).getTime();
      if (!(latest.get(c.sessionId) >= at)) latest.set(c.sessionId, at);
    }
    const lastActive = (s) => latest.get(s.id) ?? new Date(s.startedAt).getTime();
    return Array.from(sessions.values())
      .filter((s) => !isHidden(s))
      .sort((a, b) => lastActive(b) - lastActive(a));
  }

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

    const all = buildChip("", selectedSessionId === null);
    all.classList.add("chip-all");
    all.append("All", chipCount(total));
    all.addEventListener("click", () => selectSession(null));

    const chips = [all];
    for (const s of visible) {
      const selected = selectedSessionId === s.id;
      const chip = buildChip(s.id, selected);
      chip.style.setProperty("--session-colour", sessionColour(s));
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
      chip.addEventListener("click", () => selectSession(selected ? null : s.id));
      chips.push(chip);
    }

    chipsEl.replaceChildren(...chips);
    if (focusedKey !== null) {
      const again = chips.find((c) => c.dataset.sessionId === focusedKey);
      if (again) again.focus();
    }
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

  // The Settings popover hangs under the gear. Its contents are rebuilt on
  // every change while it is open; the popover element itself stays, so an
  // outside-click test against it survives the rebuild.
  const settingsToggleEl = document.getElementById("settings-toggle");
  const settingsBadgeEl = document.createElement("span");
  const settingsEl = document.getElementById("settings");
  settingsBadgeEl.className = "badge";
  settingsToggleEl.append(buildIcon("gear"), settingsBadgeEl);

  function openSettings() {
    settingsEl.hidden = false;
    settingsToggleEl.setAttribute("aria-expanded", "true");
    renderSettings();
  }

  function closeSettings({ refocus = false } = {}) {
    if (settingsEl.hidden) return;
    if (pendingConfirm && settingsEl.contains(pendingConfirm.button)) {
      clearPendingConfirm();
    }
    settingsEl.hidden = true;
    settingsToggleEl.setAttribute("aria-expanded", "false");
    if (refocus) settingsToggleEl.focus();
  }

  settingsToggleEl.addEventListener("click", () => {
    if (settingsEl.hidden) openSettings();
    else closeSettings();
  });

  document.addEventListener("click", (e) => {
    if (!clickIsInside(e, settingsEl) && !clickIsInside(e, settingsToggleEl)) {
      closeSettings();
    }
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !settingsEl.hidden) closeSettings({ refocus: true });
  });

  // A click inside a card's iframe never reaches this document; focus
  // moving into that iframe is the sign. Switching apps also blurs the
  // window but leaves activeElement outside any iframe, so Settings stays.
  window.addEventListener("blur", () => {
    if (document.activeElement instanceof HTMLIFrameElement) closeSettings();
  });

  function settingsHeading(text) {
    const h = document.createElement("h3");
    h.className = "settings-heading";
    h.textContent = text;
    return h;
  }

  function textButton(text, className, onClick) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = className;
    btn.textContent = text;
    btn.addEventListener("click", onClick);
    return btn;
  }

  function buildHiddenRow(session) {
    const row = document.createElement("div");
    row.className = "hidden-row";
    row.dataset.sessionId = session.id;

    const dot = document.createElement("span");
    dot.className = "session-dot";
    dot.style.setProperty("--session-colour", sessionColour(session));

    const text = document.createElement("div");
    text.className = "hidden-row-text";
    const name = document.createElement("div");
    name.className = "hidden-row-name";
    name.textContent = session.name;
    const meta = document.createElement("small");
    const count = sessionCardCount(session.id);
    const parts = [`${count} post${count === 1 ? "" : "s"}`];
    if (session.repo) parts.unshift(session.repo);
    if (session.endedAt) parts.push(`ended ${relativeTime(session.endedAt)}`);
    meta.textContent = parts.join(" · ");
    text.append(name, meta);

    const show = textButton("Show", "btn", () => {
      showSession(session.id);
      toast(`Showing ${session.name}`);
    });
    const del = textButton("Delete", "btn danger", () =>
      armConfirm(del, {
        idleLabel: "Delete",
        idleIcon: null,
        confirmLabel: "Confirm delete",
        confirmIcon: null,
        withText: true,
        onConfirm: () => deleteSession(session.id),
      })
    );
    del.setAttribute("aria-label", "Delete");

    row.append(dot, text, show, del);
    return row;
  }

  function renderSettings() {
    const hiddenCount = hiddenSessions().length;
    settingsBadgeEl.textContent = String(hiddenCount);
    settingsBadgeEl.hidden = hiddenCount === 0;
    if (settingsEl.hidden) return;

    // Same as a card re-render: a rebuild detaches an armed Delete, so clear
    // it and re-arm its replacement with the time it had left, so a post
    // arriving mid-confirm doesn't cancel the delete.
    let rearm = null;
    if (pendingConfirm && settingsEl.contains(pendingConfirm.button)) {
      rearm = {
        sessionId: pendingConfirm.button.closest(".hidden-row").dataset.sessionId,
        opts: pendingConfirm.opts,
        ms: pendingConfirm.deadline - Date.now(),
      };
      clearPendingConfirm();
    }
    // A Repo | Session button rebuilt under the keyboard gets focus back.
    const focusedColorBy =
      settingsEl.contains(document.activeElement) && document.activeElement.dataset.colorBy;

    settingsEl.replaceChildren();
    settingsEl.appendChild(settingsHeading("Sessions"));

    const hideEndedRow = document.createElement("label");
    hideEndedRow.className = "settings-row";
    const label = document.createElement("span");
    label.className = "settings-row-text";
    const note = document.createElement("small");
    note.textContent = "Their posts stay saved; show them again below.";
    label.append("Hide sessions when they end", note);
    const toggle = document.createElement("input");
    toggle.type = "checkbox";
    toggle.className = "switch";
    toggle.setAttribute("role", "switch");
    toggle.checked = prefs.hideEnded;
    toggle.addEventListener("change", () => setHideEnded(toggle.checked));
    hideEndedRow.append(label, toggle);
    settingsEl.appendChild(hideEndedRow);

    settingsEl.appendChild(settingsHeading("Stream"));

    const animateRow = document.createElement("label");
    animateRow.className = "settings-row";
    const animateLabel = document.createElement("span");
    animateLabel.className = "settings-row-text";
    animateLabel.textContent = "Animate new posts";
    const animateToggle = document.createElement("input");
    animateToggle.type = "checkbox";
    animateToggle.className = "switch";
    animateToggle.setAttribute("role", "switch");
    animateToggle.dataset.pref = "animate";
    animateToggle.checked = prefs.animate;
    animateToggle.addEventListener("change", () =>
      setPrefsAndRefresh(() => {
        prefs.animate = animateToggle.checked;
      })
    );
    animateRow.append(animateLabel, animateToggle);
    settingsEl.appendChild(animateRow);

    const colourRow = document.createElement("div");
    colourRow.className = "settings-row static";
    const colourLabel = document.createElement("span");
    colourLabel.className = "settings-row-text";
    colourLabel.textContent = "Colour sessions by";
    const seg = document.createElement("span");
    seg.className = "seg";
    seg.setAttribute("role", "group");
    seg.setAttribute("aria-label", "Colour sessions by");
    for (const [value, text] of [["repo", "Repo"], ["session", "Session"]]) {
      const btn = textButton(text, "seg-btn", () => setColorBy(value));
      btn.dataset.colorBy = value;
      btn.setAttribute("aria-pressed", prefs.colorBy === value ? "true" : "false");
      seg.appendChild(btn);
    }
    colourRow.append(colourLabel, seg);
    settingsEl.appendChild(colourRow);

    settingsEl.appendChild(settingsHeading(`Hidden (${hiddenCount})`));
    for (const s of hiddenSessions()) {
      settingsEl.appendChild(buildHiddenRow(s));
    }

    if (rearm && rearm.ms > 0) {
      const row = settingsEl.querySelector(
        `.hidden-row[data-session-id="${CSS.escape(rearm.sessionId)}"]`
      );
      if (row) armConfirm(row.querySelector(".btn.danger"), rearm.opts, rearm.ms);
    }
    if (focusedColorBy) {
      settingsEl.querySelector(`[data-color-by="${focusedColorBy}"]`).focus();
    }
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
    menu.appendChild(
      buildMenuItem("filter", `Show only ${sessionName(card.sessionId)}`, () =>
        showOnlySession(card.sessionId)
      )
    );
    menu.appendChild(
      buildMenuItem("eye-off", "Hide this session", () => {
        closeMenu();
        hideSession(card.sessionId);
      })
    );
    const divider = document.createElement("div");
    divider.className = "menu-divider";
    divider.setAttribute("role", "separator");
    menu.appendChild(divider);
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

  // A click inside a card's iframe never reaches this document; the window
  // losing focus to it is the only sign, so treat that as an outside click.
  window.addEventListener("blur", () => closeMenu());

  function deleteCard(id) {
    fetch(`/api/cards/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => {});
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
    const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(id)}"]`);
    if (el) el.remove();
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
      const el = cardsEl.querySelector(`[data-card-id="${CSS.escape(cardId)}"]`);
      if (el) el.remove();
    }
    closeMenuIfDetached();
    if (selectedSessionId === id) selectedSessionId = null;
    prunePrefs();
    refreshVisibility();
    toast(`Deleted ${name} and its posts`);
  }

  function cardMatchesFilter(sessionId) {
    const session = sessions.get(sessionId);
    if (session && isHidden(session)) return false;
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
    streamEl.dataset.filterSession = selectedSessionId || "";
    updateEmptyState(visible);
  }

  // The empty-state message differs depending on whether every session is
  // hidden, the selected chip has no posts, or there are no posts at all.
  function updateEmptyState(visible) {
    emptyStateEl.hidden = visible > 0;
    if (visible > 0) return;
    const sessionList = Array.from(sessions.values());
    if (sessionList.length > 0 && sessionList.every(isHidden)) {
      // "show ended sessions" only helps when the ended rule hid one of
      // them; sessions hidden by hand come back one at a time in Settings.
      const endedHidden = sessionList.some((s) => !prefs.hidden.includes(s.id));
      if (endedHidden) {
        emptyStateEl.replaceChildren(
          "No open sessions. Posts from sessions that ended are kept — ",
          textButton("show ended sessions", "link-btn", () => setHideEnded(false)),
          "."
        );
      } else {
        emptyStateEl.replaceChildren(
          "All sessions are hidden. ",
          // Stopped here, or the page-wide outside-click handler sees a
          // click outside the popover it just opened and closes it again.
          textButton("Open Settings", "link-btn", (e) => {
            e.stopPropagation();
            openSettings();
          })
        );
      }
    } else if (selectedSessionId !== null) {
      emptyStateEl.replaceChildren(
        "No posts from this session yet. ",
        textButton("Show all", "link-btn", () => selectSession(null))
      );
    } else {
      emptyStateEl.textContent =
        "No posts yet. Canvas shows what your Claude sessions post as they work.";
    }
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

  function buildIframeDoc(html) {
    html = absolutizeCardImageSrcs(html);
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
      </script>
    `;
    return (
      `<!doctype html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
      `<style>html,body{margin:0;overflow:hidden;}` +
      `body{font-family:-apple-system,sans-serif;}` +
      `img[src*="/api/cards/"]:not(a img){cursor:zoom-in;}` +
      `#__canvas_root{overflow:hidden;}</style>` +
      `</head><body><div id="__canvas_root">${html}</div>${resizeScript}</body></html>`
    );
  }

  // Finds the card whose iframe's contentWindow is `source` — the only way
  // a card is identified from a postMessage, since the message itself never
  // carries a card id (an untrusted iframe naming its own card id would let
  // one card's script open another card's targets).
  function cardForFrameSource(source) {
    const frames = cardsEl.querySelectorAll("iframe");
    for (const frame of frames) {
      if (frame.contentWindow === source) {
        const el = frame.closest(".card");
        return el ? cards.get(el.dataset.cardId) : null;
      }
    }
    return null;
  }

  window.addEventListener("message", (event) => {
    const data = event.data;
    if (!data) return;

    if (data.type === "canvas-resize") {
      const frames = cardsEl.querySelectorAll("iframe");
      for (const frame of frames) {
        if (frame.contentWindow === event.source) {
          const cardEl = frame.closest(".card");
          const wasAbove = cardIsAboveViewport(cardEl);
          const before = frame.offsetHeight;
          frame.style.height = `${Math.max(20, data.height)}px`;
          if (wasAbove) holdPlace(frame.offsetHeight - before);
          break;
        }
      }
      return;
    }

    if (data.type === "canvas-open") {
      // Only a real card iframe's contentWindow may trigger an open — never
      // the top window itself, and never an index a card iframe cannot
      // name a path for.
      const card = cardForFrameSource(event.source);
      if (!card) return;
      const index = data.index;
      if (!Number.isInteger(index) || index < 0 || index >= card.targets.length) {
        return;
      }
      const path = card.targets[index];
      fetch("/api/open", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ path }),
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
      cardsEl.appendChild(renderCard(card));
    }
    refreshVisibility();
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
      // The replacement is about to detach this card's menu too — close it
      // first (which also clears an armed Delete post inside it), so neither
      // openMenu nor pendingConfirm holds a node no longer in the document.
      existing.remove();
      closeMenuIfDetached();
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

    // Only a new post from a session the reader can see arrives; replacing
    // a card, or a post from a hidden session, is silent.
    const session = sessions.get(card.sessionId);
    const arrives = !existing && !(session && isHidden(session));
    if (arrives && !el.hidden) arrive(el, card);

    // A new card changes its session's count — refresh the chip row and
    // Settings without touching any other card, so a card for a
    // non-selected session updates its chip's count and leaves the visible
    // stream alone.
    refreshVisibility();

    if (arrives && !el.hidden) pulseChip(card.sessionId);
    if (arrives && el.hidden) {
      toast(`New post from ${sessionName(card.sessionId)} (filtered out)`, "Show", () => {
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
      unseen.count++;
      unseen.sessionIds.add(card.sessionId);
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
    if (!chip) return;
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
    unseen.count = 0;
    unseen.sessionIds.clear();
    renderPill();
  }

  // "↑", up to four dots in the colours of the sessions with unseen posts,
  // and the count.
  function renderPill() {
    newPillEl.hidden = unseen.count === 0;
    newPillEl.dataset.count = String(unseen.count);
    if (unseen.count === 0) return;
    const dots = document.createElement("span");
    dots.className = "pill-dots";
    for (const id of Array.from(unseen.sessionIds).slice(-4)) {
      const s = sessions.get(id);
      if (!s) continue;
      const dot = document.createElement("span");
      dot.className = "session-dot";
      dot.style.setProperty("--session-colour", sessionColour(s));
      dots.appendChild(dot);
    }
    const n = unseen.count;
    newPillEl.replaceChildren("↑", dots, `${n} new post${n === 1 ? "" : "s"}`);
  }

  newPillEl.addEventListener("click", scrollToTop);
  toTopEl.appendChild(buildIcon("arrow-up"));
  toTopEl.addEventListener("click", scrollToTop);

  streamEl.addEventListener("scroll", () => {
    const y = streamEl.scrollTop;
    toTopEl.hidden = y <= TO_TOP_PX;
    if (y < SCROLLED_PX) {
      returningToTop = false;
      if (unseen.count > 0) clearUnseen();
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
    for (const el of cardsEl.querySelectorAll(card)) {
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

  function paintLightbox() {
    const { card, index } = lightbox;
    const count = card.images.length;
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
    overlayEl.hidden = true;
    overlayImgEl.src = "";
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
  overlayEl.addEventListener("click", closeLightbox);
  window.addEventListener("keydown", (e) => {
    if (!lightbox) return;
    if (e.key === "Escape") closeLightbox();
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
    for (const c of data.cards) cards.set(c.id, c);
    prunePrefs();
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
      const before = sessions.get(session.id);
      sessions.set(session.id, session);
      sessionColour(session);
      refreshVisibility();
      patchSessionNames(session.id);
      // Only the upsert that first sets endedAt on a session this viewer was
      // showing announces the hide; a reload finds it hidden silently.
      const endedJustNow = before && !before.endedAt && session.endedAt;
      if (endedJustNow && !isHidden(before) && isHidden(session)) {
        toast(`${session.name} ended — its posts are hidden`, "Keep showing", () =>
          showSession(session.id)
        );
      }
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
