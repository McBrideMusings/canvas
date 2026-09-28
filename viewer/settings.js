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
  const colourButtons = Array.from(document.querySelectorAll("[data-color-by]"));

  // Daemon status: only Canvas.app's WKWebView injects window.__TAURI__ (see
  // app.js's own get_pinned/open_settings calls) — a plain browser tab has
  // nothing to report here, since it's canvasd itself answering the page.
  const daemonPrefEl = document.getElementById("daemon-status-pref");
  const daemonDotEl = document.getElementById("daemon-status-dot");
  const daemonTextEl = document.getElementById("daemon-status-text");
  const daemonPathEl = document.getElementById("daemon-status-path");
  const daemonErrorEl = document.getElementById("daemon-status-error");

  function renderDaemonStatus(status) {
    daemonPathEl.textContent = status.installedPath;
    if (status.error) {
      daemonDotEl.dataset.state = "error";
      daemonTextEl.textContent = "Needs attention";
      daemonErrorEl.textContent = status.error;
      daemonErrorEl.hidden = false;
    } else if (status.running && status.upToDate) {
      daemonDotEl.dataset.state = "ok";
      daemonTextEl.textContent = "Running";
      daemonErrorEl.hidden = true;
    } else if (status.running && !status.upToDate) {
      daemonDotEl.dataset.state = "";
      daemonTextEl.textContent = "Running (older than this app — reopen Canvas to update it)";
      daemonErrorEl.hidden = true;
    } else if (status.loaded) {
      daemonDotEl.dataset.state = "error";
      daemonTextEl.textContent = "Installed but not running";
      daemonErrorEl.hidden = true;
    } else if (status.installed) {
      daemonDotEl.dataset.state = "error";
      daemonTextEl.textContent = "Installed but not registered to run";
      daemonErrorEl.hidden = true;
    } else {
      daemonDotEl.dataset.state = "error";
      daemonTextEl.textContent = "Not installed";
      daemonErrorEl.hidden = true;
    }
  }

  function pollDaemonStatus(tauriCore) {
    tauriCore
      .invoke("daemon_status")
      .then(renderDaemonStatus)
      .catch(() => {});
  }

  if (window.__TAURI__ && window.__TAURI__.core) {
    daemonPrefEl.hidden = false;
    pollDaemonStatus(window.__TAURI__.core);
    setInterval(() => pollDaemonStatus(window.__TAURI__.core), 5000);
  }

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
    if (section === "profiles") loadProfilesTab();
  }

  for (const tab of tabs) {
    tab.addEventListener("click", () => openTab(tab.dataset.section));
  }

  // --- Profiles tab: backed by canvasd, not localStorage. Generalizes what
  // used to be a single-purpose "guidance" override into named profiles per
  // kind (e.g. posting-guidance, dashboard-style), each assignable globally
  // or per repo — read by every session's SessionStart hook and by anything
  // that wants a preconfigured style on this Mac, not just this browser tab.
  const kindButtons = Array.from(document.querySelectorAll("[data-kind]"));
  const profilesStatusEl = document.getElementById("profiles-status");
  const profilesListEl = document.getElementById("profiles-list");
  const profileNewNameEl = document.getElementById("profile-new-name");
  const profileNewAddEl = document.getElementById("profile-new-add");
  const profileGlobalSelectEl = document.getElementById("profile-global-select");
  const profileGlobalStatusEl = document.getElementById("profile-global-status");
  const profileRepoInputEl = document.getElementById("profile-repo-input");
  const profileRepoListEl = document.getElementById("profile-repo-list");
  const profileRepoSelectEl = document.getElementById("profile-repo-select");
  const profileRepoStatusEl = document.getElementById("profile-repo-status");

  let currentKind = "posting-guidance";
  let knownRepos = [];

  async function getJson(url) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return response.json();
  }

  async function putJson(url, body) {
    const response = await fetch(url, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  }

  function flashStatus(el, text) {
    el.textContent = text;
    clearTimeout(el._timer);
    el._timer = setTimeout(() => {
      el.textContent = "";
    }, 2000);
  }

  function fillProfileSelect(select, profileNames, selected) {
    select.innerHTML = "";
    const none = document.createElement("option");
    none.value = "";
    none.textContent = "Built-in default";
    select.appendChild(none);
    for (const name of profileNames) {
      const option = document.createElement("option");
      option.value = name;
      option.textContent = name;
      select.appendChild(option);
    }
    select.value = selected || "";
  }

  function renderProfileRows(profiles) {
    profilesListEl.innerHTML = "";
    const names = Object.keys(profiles).sort();
    for (const name of names) {
      const row = document.createElement("div");
      row.className = "profile-row";

      const head = document.createElement("div");
      head.className = "profile-row-head";
      const label = document.createElement("span");
      label.className = "profile-row-name";
      label.textContent = name;
      const deleteBtn = document.createElement("button");
      deleteBtn.type = "button";
      deleteBtn.className = "btn btn-danger";
      deleteBtn.textContent = "Delete";
      head.append(label, deleteBtn);

      const textarea = document.createElement("textarea");
      textarea.className = "guidance-text";
      textarea.rows = 5;
      textarea.value = profiles[name];

      const actions = document.createElement("div");
      actions.className = "guidance-actions";
      const saveBtn = document.createElement("button");
      saveBtn.type = "button";
      saveBtn.className = "btn";
      saveBtn.textContent = "Save";
      const status = document.createElement("span");
      status.className = "guidance-status";
      actions.append(saveBtn, status);

      saveBtn.addEventListener("click", async () => {
        try {
          await putJson(`/api/profiles/${currentKind}/definitions`, {
            name,
            text: textarea.value,
          });
          if (!textarea.value.trim()) {
            // Blank text is how this route deletes a profile — reload so the
            // row (and any select showing it as assigned) doesn't keep
            // pointing at a name that no longer exists.
            flashStatus(profilesStatusEl, `"${name}" deleted (blank text)`);
            await loadProfilesTab(true);
            return;
          }
          flashStatus(status, "saved");
        } catch (e) {
          flashStatus(status, "save failed");
        }
      });

      // Deleting is permanent (it also clears any global/repo assignment
      // pointing at this profile) and has no undo, so the first click only
      // arms the button — a second click within 3s is what actually deletes.
      deleteBtn.addEventListener("click", async () => {
        if (deleteBtn.dataset.armed !== "true") {
          deleteBtn.dataset.armed = "true";
          deleteBtn.textContent = "Confirm delete?";
          clearTimeout(deleteBtn._disarmTimer);
          deleteBtn._disarmTimer = setTimeout(() => {
            deleteBtn.dataset.armed = "false";
            deleteBtn.textContent = "Delete";
          }, 3000);
          return;
        }
        clearTimeout(deleteBtn._disarmTimer);
        try {
          await putJson(`/api/profiles/${currentKind}/definitions`, {
            name,
            text: null,
          });
          await loadProfilesTab(true);
        } catch (e) {
          flashStatus(profilesStatusEl, "delete failed");
        }
      });

      row.append(head, textarea, actions);
      profilesListEl.appendChild(row);
    }
  }

  async function loadKnownRepos() {
    try {
      const state = await getJson("/api/state");
      knownRepos = Array.from(
        new Set(state.sessions.map((s) => s.repo).filter(Boolean))
      ).sort();
      profileRepoListEl.innerHTML = "";
      for (const repo of knownRepos) {
        const option = document.createElement("option");
        option.value = repo;
        profileRepoListEl.appendChild(option);
      }
    } catch (e) {}
  }

  async function loadProfilesTab(force) {
    if (!force && loadProfilesTab._loadedKind === currentKind && knownRepos.length) return;
    loadProfilesTab._loadedKind = currentKind;

    await loadKnownRepos();

    try {
      const p = await getJson(`/api/profiles/${currentKind}`);
      const names = Object.keys(p.profiles).sort();
      renderProfileRows(p.profiles);
      fillProfileSelect(profileGlobalSelectEl, names, p.global);
      const repo = profileRepoInputEl.value.trim();
      fillProfileSelect(profileRepoSelectEl, names, repo ? p.repos[repo] : "");
      profileRepoSelectEl.disabled = !repo;
    } catch (e) {
      flashStatus(profilesStatusEl, "could not reach canvasd");
    }
  }

  for (const btn of kindButtons) {
    btn.addEventListener("click", () => {
      for (const b of kindButtons) b.setAttribute("aria-pressed", String(b === btn));
      currentKind = btn.dataset.kind;
      profileRepoInputEl.value = "";
      profileRepoSelectEl.disabled = true;
      loadProfilesTab(true);
    });
  }

  profileNewAddEl.addEventListener("click", async () => {
    const name = profileNewNameEl.value.trim();
    if (!name) return;
    try {
      // Blank text is how the definitions route deletes a profile, so a new
      // one needs a real starter value to actually get created — the user
      // overwrites it in the textarea that appears right after.
      await putJson(`/api/profiles/${currentKind}/definitions`, {
        name,
        text: "(write this profile's text)",
      });
      profileNewNameEl.value = "";
      await loadProfilesTab(true);
    } catch (e) {
      flashStatus(profilesStatusEl, "add failed");
    }
  });

  profileGlobalSelectEl.addEventListener("change", async () => {
    try {
      await putJson(`/api/profiles/${currentKind}/global`, {
        profile: profileGlobalSelectEl.value || null,
      });
      flashStatus(profileGlobalStatusEl, "saved");
    } catch (e) {
      flashStatus(profileGlobalStatusEl, "save failed");
    }
  });

  async function loadRepoAssignment() {
    const repo = profileRepoInputEl.value.trim();
    if (!repo) {
      profileRepoSelectEl.disabled = true;
      return;
    }
    profileRepoSelectEl.disabled = false;
    try {
      const p = await getJson(
        `/api/profiles/${currentKind}?repo=${encodeURIComponent(repo)}`
      );
      fillProfileSelect(profileRepoSelectEl, Object.keys(p.profiles).sort(), p.repos[repo]);
    } catch (e) {}
  }

  profileRepoInputEl.addEventListener("change", loadRepoAssignment);
  profileRepoInputEl.addEventListener("blur", loadRepoAssignment);

  profileRepoSelectEl.addEventListener("change", async () => {
    const repo = profileRepoInputEl.value.trim();
    if (!repo) return;
    try {
      await putJson(`/api/profiles/${currentKind}/repos`, {
        repo,
        profile: profileRepoSelectEl.value || null,
      });
      flashStatus(profileRepoStatusEl, "saved");
    } catch (e) {
      flashStatus(profileRepoStatusEl, "save failed");
    }
  });

  function render() {
    const stored = readStored();
    archiveEnded.checked = typeof stored.hideEnded === "boolean" ? stored.hideEnded : true;
    animate.checked = typeof stored.animate === "boolean" ? stored.animate : true;
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

  // A URL hash (`#profiles`) opens straight to that section instead of the
  // General default — the gear icon per section, and a way to link/script a
  // specific settings pane without clicking through the tab bar. Runs last
  // so every element and function this section touches is already defined.
  if (sections.has(location.hash.slice(1))) {
    openTab(location.hash.slice(1));
  }
})();
