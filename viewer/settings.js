(() => {
  "use strict";

  // The viewer's preferences: one JSON object in the key the main window
  // reads (viewer/app.js). This page owns two fields of it. Each change reads
  // the stored object fresh and writes it back with only that field changed,
  // so the archived and shown lists the main window keeps there survive.
  const PREFS_KEY = "canvas.viewer";

  function readStored() {
    try {
      const parsed = JSON.parse(localStorage.getItem(PREFS_KEY) || "{}");
      if (parsed && typeof parsed === "object") return parsed;
    } catch (e) {}
    return {};
  }

  function change(edit) {
    const stored = readStored();
    edit(stored);
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(stored));
    } catch (e) {}
  }

  const archiveEnded = document.getElementById("archive-ended");
  const animate = document.getElementById("animate");
  const fullWidth = document.getElementById("full-width");
  const colourButtons = Array.from(document.querySelectorAll("[data-color-by]"));

  // Daemon tab: only Canvas.app's WKWebView injects window.__TAURI__ (see
  // app.js's own get_keep_on_top/open_settings calls) — a plain browser tab has
  // nothing to report here, since it's canvasd itself answering the page.
  // Every fact daemon_status returns gets its own row rather than folding
  // them into one dot: a daemon can be installed but not registered with
  // launchd, registered but not running, or running an older binary than
  // this app bundles, and those are different problems with different fixes.
  const daemonTabEl = document.getElementById("tab-daemon");
  const daemonDotEl = document.getElementById("daemon-status-dot");
  const daemonTextEl = document.getElementById("daemon-status-text");
  const daemonPathEl = document.getElementById("daemon-status-path");
  const daemonErrorEl = document.getElementById("daemon-status-error");
  const daemonRefreshEl = document.getElementById("daemon-refresh");
  const daemonCheckedAtEl = document.getElementById("daemon-checked-at");
  const daemonRows = {
    installed: [document.getElementById("daemon-installed-dot"), document.getElementById("daemon-installed-text")],
    upToDate: [document.getElementById("daemon-uptodate-dot"), document.getElementById("daemon-uptodate-text")],
    loaded: [document.getElementById("daemon-loaded-dot"), document.getElementById("daemon-loaded-text")],
    running: [document.getElementById("daemon-running-dot"), document.getElementById("daemon-running-text")],
  };

  // Every [data-relaunch] button and the Integrations notice show together,
  // while the bundled canvas differs from the installed one. Relaunching
  // restarts the app (and with it the daemon), so the page never sees the reply.
  const relaunchButtons = Array.from(document.querySelectorAll("[data-relaunch]"));
  const integrationsRelaunchEl = document.getElementById("integrations-relaunch");
  const relaunchErrors = Array.from(document.querySelectorAll("[data-relaunch-error]"));

  function showRelaunchError(failed) {
    for (const el of relaunchErrors) el.hidden = !failed;
  }

  function showRelaunch(needed) {
    for (const btn of relaunchButtons) {
      if (btn.closest("#integrations-relaunch")) continue;
      btn.hidden = !needed;
    }
    integrationsRelaunchEl.hidden = !needed;
    if (!needed) showRelaunchError(false);
  }

  for (const btn of relaunchButtons) {
    btn.addEventListener("click", () => {
      showRelaunchError(false);
      for (const b of relaunchButtons) b.disabled = true;
      window.__TAURI__.core.invoke("relaunch").catch(() => {
        for (const b of relaunchButtons) b.disabled = false;
        showRelaunchError(true);
      });
    });
  }

  function setRow(row, ok, yes, no) {
    const [dot, text] = row;
    dot.dataset.state = ok ? "ok" : "error";
    text.textContent = ok ? yes : no;
  }

  function renderDaemonStatus(status) {
    daemonPathEl.textContent = status.installedPath;
    setRow(daemonRows.installed, status.installed, "Yes", "No");
    setRow(daemonRows.upToDate, status.upToDate, "Yes", status.relaunchNeeded ? "No — relaunch to update it" : "No");
    showRelaunch(status.relaunchNeeded);
    setRow(daemonRows.loaded, status.loaded, "Yes", "No");
    setRow(daemonRows.running, status.running, "Yes", "No");
    daemonCheckedAtEl.textContent = new Date().toLocaleTimeString();

    if (status.error) {
      daemonDotEl.dataset.state = "error";
      daemonTextEl.textContent = "Needs attention";
      daemonErrorEl.textContent = status.error;
      daemonErrorEl.hidden = false;
    } else {
      daemonErrorEl.hidden = true;
      if (status.running && status.upToDate) {
        daemonDotEl.dataset.state = "ok";
        daemonTextEl.textContent = "Running";
      } else if (status.running) {
        daemonDotEl.dataset.state = "";
        daemonTextEl.textContent = "Running an older build";
      } else if (status.loaded) {
        daemonDotEl.dataset.state = "error";
        daemonTextEl.textContent = "Installed but not running";
      } else if (status.installed) {
        daemonDotEl.dataset.state = "error";
        daemonTextEl.textContent = "Installed but not registered to run";
      } else {
        daemonDotEl.dataset.state = "error";
        daemonTextEl.textContent = "Not installed";
      }
    }
  }

  function pollDaemonStatus() {
    if (!window.__TAURI__) return;
    window.__TAURI__.core
      .invoke("daemon_status")
      .then(renderDaemonStatus)
      .catch(() => {
        daemonTextEl.textContent = "Could not reach the app's own backend";
      });
  }

  let daemonPollTimer = null;

  function startDaemonPolling() {
    pollDaemonStatus();
    clearInterval(daemonPollTimer);
    daemonPollTimer = setInterval(pollDaemonStatus, 5000);
  }

  function stopDaemonPolling() {
    clearInterval(daemonPollTimer);
    daemonPollTimer = null;
  }

  if (window.__TAURI__ && window.__TAURI__.core) {
    daemonRefreshEl.addEventListener("click", pollDaemonStatus);
  } else {
    daemonTabEl.hidden = true;
  }

  // Integrations tab: one row per coding agent, from the app's
  // integration_status (which runs `canvas integrations list --json`). A row
  // is one of: not found, current, out of date, installing (this page's own
  // click, or the app's `installing` flag for a launch-time install or a
  // Retry queued behind one), needs review, or install failed
  // with the CLI's stderr line. Only Canvas.app can answer, so outside it the
  // tab is hidden like Daemon's.
  const integrationsTabEl = document.getElementById("tab-integrations");
  const integrationsListEl = document.getElementById("integrations-list");
  const integrationsErrorEl = document.getElementById("integrations-load-error");
  const integrationsCheckedEl = document.getElementById("integrations-checked");
  const integrationsRefreshEl = document.getElementById("integrations-refresh");
  const AGENT_NAMES = { "claude-code": "Claude Code", codex: "Codex" };
  const AGENT_COMMANDS = { "claude-code": "claude", codex: "codex" };
  const GLYPHS = {
    current: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
    stale: '<circle cx="12" cy="12" r="8.5"/><path d="M12 16V8M8.5 11.5L12 8l3.5 3.5"/>',
    notfound: '<circle cx="12" cy="12" r="8.5"/><path d="M8 12h8"/>',
    error: '<path d="M12 4l9 16H3z"/><path d="M12 10v4M12 17v.01"/>',
    busy: '<path d="M12 3.5a8.5 8.5 0 108.5 8.5"/>',
    review: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="2.5"/>',
  };
  let integrationRows = [];
  let integrationsPoll = null;
  const installing = new Set();

  // What one row shows: its state, the phrase, an optional hint or error,
  // and the button label (none when there is nothing to do).
  function integrationView(row) {
    const name = AGENT_NAMES[row.agent] || row.agent;
    if (installing.has(row.agent) || row.installing) return { state: "busy", phrase: "Installing…", action: "Installing…" };
    if (!row.found) {
      const cmd = AGENT_COMMANDS[row.agent] || row.agent;
      return { state: "notfound", phrase: "Not found", hint: `The ${cmd} command isn't on your PATH. Canvas sets it up once you install it.` };
    }
    if (row.error) return { state: "error", phrase: "Install failed", error: row.error, action: "Retry" };
    switch (row.status) {
      case "current":
        return { state: "current", phrase: "Installed · Current" };
      case "out of date":
        return { state: "stale", phrase: "Installed · Out of date", action: "Update" };
      case "needs review":
        return { state: "review", phrase: "Installed · Needs review", hint: `Open ${name} and trust Canvas's hooks when it asks (“Hooks need review”).` };
      case "not installed":
        return { state: "stale", phrase: "Not installed", action: "Install" };
      default:
        return { state: "stale", phrase: row.status || "Unknown" };
    }
  }

  function integrationRowEl(row) {
    const view = integrationView(row);
    const name = AGENT_NAMES[row.agent] || row.agent;
    const el = document.createElement("div");
    el.className = "integration-row";
    el.dataset.state = view.state;
    el.dataset.agent = row.agent;

    const glyph = document.createElement("span");
    glyph.className = "integration-glyph";
    glyph.setAttribute("aria-hidden", "true");
    glyph.innerHTML = `<svg viewBox="0 0 24 24">${GLYPHS[view.state]}</svg>`;

    const body = document.createElement("div");
    body.className = "integration-body";
    const nameEl = document.createElement("div");
    nameEl.className = "integration-name";
    nameEl.textContent = name;
    const phraseEl = document.createElement("div");
    phraseEl.className = "integration-phrase";
    phraseEl.textContent = view.phrase;
    body.append(nameEl, phraseEl);
    if (view.error) {
      const errEl = document.createElement("pre");
      errEl.className = "daemon-error";
      errEl.setAttribute("role", "alert");
      errEl.textContent = view.error;
      body.append(errEl);
    }
    if (view.hint) {
      const hintEl = document.createElement("p");
      hintEl.className = "pref-note";
      hintEl.textContent = view.hint;
      body.append(hintEl);
    }
    el.append(glyph, body);

    if (view.action) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = view.state === "error" || view.state === "busy" ? "btn" : "btn btn-secondary";
      btn.textContent = view.action;
      btn.disabled = view.state === "busy";
      btn.setAttribute("aria-label", `${view.state === "busy" ? "Installing" : view.action} ${name}`);
      btn.addEventListener("click", () => installIntegration(row.agent));
      el.append(btn);
    }
    return el;
  }

  function renderIntegrations() {
    integrationsListEl.replaceChildren();
    if (integrationRows.length > 0 && integrationRows.every((r) => !r.found)) {
      const empty = document.createElement("div");
      empty.className = "integrations-empty";
      const title = document.createElement("b");
      title.textContent = "No coding agents found";
      empty.append(title, "Canvas looks for claude and codex on your PATH. Install one and press Check now.");
      integrationsListEl.append(empty);
      return;
    }
    for (const row of integrationRows) integrationsListEl.append(integrationRowEl(row));
  }

  // An install the app is running (launch time, or a queued Retry) ends
  // without this page being told, so check again until none is. Only while
  // the Integrations tab is showing in a visible window.
  function scheduleIntegrationsPoll(again) {
    clearTimeout(integrationsPoll);
    if (again && !document.hidden && integrationsTabEl.getAttribute("aria-selected") === "true") {
      integrationsPoll = setTimeout(refreshIntegrations, 2000);
    }
  }

  function refreshIntegrations() {
    if (!window.__TAURI__) return Promise.resolve();
    window.__TAURI__.core
      .invoke("daemon_status")
      .then((status) => showRelaunch(status.relaunchNeeded))
      .catch(() => {});
    return window.__TAURI__.core
      .invoke("integration_status")
      .then((rows) => {
        integrationRows = rows;
        integrationsErrorEl.hidden = true;
        integrationsCheckedEl.textContent = `Checked ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`;
        renderIntegrations();
        scheduleIntegrationsPoll(rows.some((r) => r.installing));
      })
      .catch((e) => {
        integrationsErrorEl.textContent = String(e);
        integrationsErrorEl.hidden = false;
        scheduleIntegrationsPoll(integrationRows.some((r) => r.installing));
      });
  }

  function installIntegration(agent) {
    installing.add(agent);
    renderIntegrations();
    window.__TAURI__.core
      .invoke("integration_install", { agent })
      .catch(() => {})
      .then(() => {
        installing.delete(agent);
        return refreshIntegrations();
      })
      .then(renderIntegrations);
  }

  if (window.__TAURI__ && window.__TAURI__.core) {
    integrationsRefreshEl.addEventListener("click", refreshIntegrations);
  } else {
    integrationsTabEl.hidden = true;
  }

  // The settings window hides rather than closes (lib.rs), so this script
  // keeps running — pause the poll while the page isn't visible instead of
  // ticking a background window forever.
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) {
      stopDaemonPolling();
      clearTimeout(integrationsPoll);
    } else if (integrationsTabEl.getAttribute("aria-selected") === "true") {
      refreshIntegrations();
    } else if (daemonTabEl.getAttribute("aria-selected") === "true") startDaemonPolling();
  });

  // Section tabs: one visible <main> at a time, no persisted selection —
  // every open starts on General, same as the window's own default size.
  const tabs = Array.from(document.querySelectorAll(".settings-tab"));
  const titleEl = document.getElementById("settings-title");
  const sections = new Map(
    tabs.map((tab) => [tab.dataset.section, document.getElementById(`section-${tab.dataset.section}`)])
  );

  function selectTab(section) {
    for (const tab of tabs) {
      const selected = tab.dataset.section === section;
      tab.setAttribute("aria-selected", String(selected));
      sections.get(tab.dataset.section).hidden = !selected;
      if (selected) titleEl.textContent = tab.textContent.trim();
    }
  }

  function openTab(section) {
    selectTab(section);
    if (section === "instructions" || section === "reminders") LayerPage.open(section);
    if (section === "integrations") refreshIntegrations();
    if (section === "daemon") startDaemonPolling();
    else stopDaemonPolling();
  }

  for (const tab of tabs) {
    tab.addEventListener("click", () => openTab(tab.dataset.section));
  }

  function render() {
    const stored = readStored();
    archiveEnded.checked = typeof stored.hideEnded === "boolean" ? stored.hideEnded : true;
    animate.checked = typeof stored.animate === "boolean" ? stored.animate : true;
    fullWidth.checked = typeof stored.fullWidth === "boolean" ? stored.fullWidth : false;
    const colorBy = stored.colorBy === "session" ? "session" : "repo";
    for (const btn of colourButtons) {
      btn.setAttribute("aria-pressed", btn.dataset.colorBy === colorBy ? "true" : "false");
    }
  }

  archiveEnded.addEventListener("change", () => {
    change((stored) => {
      stored.hideEnded = archiveEnded.checked;
      // Off, every ended session is visible anyway; clearing shown means
      // turning it back on archives all of them again, not a remembered few.
      if (!archiveEnded.checked) stored.shown = [];
    });
  });

  animate.addEventListener("change", () => {
    change((stored) => {
      stored.animate = animate.checked;
    });
  });

  fullWidth.addEventListener("change", () => {
    change((stored) => {
      stored.fullWidth = fullWidth.checked;
    });
  });

  for (const btn of colourButtons) {
    btn.addEventListener("click", () => {
      change((stored) => {
        stored.colorBy = btn.dataset.colorBy;
      });
      render();
    });
  }

  window.addEventListener("storage", render);
  window.addEventListener("focus", render);
  render();

  // A URL hash (`#reminders`) opens straight to that section instead of the
  // General default — the gear icon per section, and a way to link/script a
  // specific settings pane without clicking through the tab bar. Runs last
  // so every element and function this section touches is already defined.
  if (sections.has(location.hash.slice(1))) {
    openTab(location.hash.slice(1));
  }
})();
