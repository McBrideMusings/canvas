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

  // Daemon tab: only Canvas.app's WKWebView injects window.__TAURI__ (see
  // app.js's own get_pinned/open_settings calls) — a plain browser tab has
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

  function setRow(row, ok, yes, no) {
    const [dot, text] = row;
    dot.dataset.state = ok ? "ok" : "error";
    text.textContent = ok ? yes : no;
  }

  function renderDaemonStatus(status) {
    daemonPathEl.textContent = status.installedPath;
    setRow(daemonRows.installed, status.installed, "Yes", "No");
    setRow(daemonRows.upToDate, status.upToDate, "Yes", "No — reopen Canvas to update it");
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

  // The settings window hides rather than closes (lib.rs), so this script
  // keeps running — pause the poll while the page isn't visible instead of
  // ticking a background window forever.
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stopDaemonPolling();
    else if (daemonTabEl.getAttribute("aria-selected") === "true") startDaemonPolling();
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
    if (section === "guidance") loadProfilesTab();
    if (section === "daemon") startDaemonPolling();
    else stopDaemonPolling();
  }

  for (const tab of tabs) {
    tab.addEventListener("click", () => openTab(tab.dataset.section));
  }

  // --- Guidance tab: backed by canvasd, not localStorage. The store
  // (canvasd/src/profiles.rs) is generalized to hold more than one "kind" of
  // named profile, but posting-guidance is the only one that ships — no
  // kind switcher here until a second kind has a real consumer.
  const KIND = "posting-guidance";
  const profilesStatusEl = document.getElementById("profiles-status");
  const profilesListEl = document.getElementById("profiles-list");
  const profileNewEl = document.getElementById("profile-new");
  const profileGlobalSelectEl = document.getElementById("profile-global-select");
  const profileGlobalStatusEl = document.getElementById("profile-global-status");
  const profileRepoInputEl = document.getElementById("profile-repo-input");
  const profileRepoListEl = document.getElementById("profile-repo-list");
  const profileRepoSelectEl = document.getElementById("profile-repo-select");
  const profileRepoStatusEl = document.getElementById("profile-repo-status");
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

  function iconButton(className, title, pathD) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = className;
    btn.title = title;
    btn.setAttribute("aria-label", title);
    btn.innerHTML =
      `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" ` +
      `stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">` +
      `<path d="${pathD}" /></svg>`;
    return btn;
  }

  const TRASH_PATH =
    "M4 7h16M9 7V4a1 1 0 011-1h4a1 1 0 011 1v3M6 7l1 13a1 1 0 001 1h8a1 1 0 001-1l1-13M10 11v6M14 11v6";
  const PENCIL_PATH = "M16.5 3.5a1.5 1.5 0 012.12 2.12L7 17.25 3 18l.75-4L15.38 3.38a1.5 1.5 0 011.12-.5z";

  // The compiled-in default a session falls back to when nothing is
  // assigned — shown in the same list as the editable profiles, collapsible
  // the same way, but with no rename/delete/save: it isn't stored data,
  // it's the text this build of Canvas shipped with.
  function renderBuiltinRow(text) {
    const row = document.createElement("div");
    row.className = "profile-row";

    const head = document.createElement("div");
    head.className = "profile-row-head";
    head.setAttribute("role", "button");
    head.setAttribute("tabindex", "0");
    head.setAttribute("aria-expanded", "false");
    const chevron = document.createElement("span");
    chevron.className = "profile-row-chevron";
    chevron.textContent = "▸";
    chevron.setAttribute("aria-hidden", "true");
    const label = document.createElement("span");
    label.className = "profile-row-name";
    label.textContent = "Built-in default";
    const lock = document.createElement("span");
    lock.className = "profile-row-readonly-tag";
    lock.textContent = "read-only";
    const preview = document.createElement("span");
    preview.className = "profile-row-preview";
    preview.textContent = text;
    head.append(chevron, label, lock, preview);

    const detail = document.createElement("div");
    detail.className = "profile-row-detail";
    detail.hidden = true;
    const textarea = document.createElement("textarea");
    textarea.className = "guidance-text";
    textarea.rows = 5;
    textarea.value = text;
    textarea.disabled = true;
    const note = document.createElement("p");
    note.className = "pref-note";
    note.textContent = "Compiled into this build of Canvas — edit plugin/guidance.md to change it.";
    detail.append(textarea, note);

    const toggle = () => {
      const expanded = head.getAttribute("aria-expanded") === "true";
      head.setAttribute("aria-expanded", String(!expanded));
      detail.hidden = expanded;
    };
    head.addEventListener("click", toggle);
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    });

    row.append(head, detail);
    profilesListEl.appendChild(row);
  }

  function renderProfileRows(profiles, builtin) {
    profilesListEl.innerHTML = "";
    if (builtin) renderBuiltinRow(builtin);
    const names = Object.keys(profiles).sort();
    for (const name of names) {
      const row = document.createElement("div");
      row.className = "profile-row";

      const head = document.createElement("div");
      head.className = "profile-row-head";
      head.setAttribute("role", "button");
      head.setAttribute("tabindex", "0");
      head.setAttribute("aria-expanded", "false");
      const chevron = document.createElement("span");
      chevron.className = "profile-row-chevron";
      chevron.textContent = "▸";
      chevron.setAttribute("aria-hidden", "true");
      const label = document.createElement("span");
      label.className = "profile-row-name";
      label.textContent = name;
      const renameBtn = iconButton("icon-btn", "Rename", PENCIL_PATH);
      const preview = document.createElement("span");
      preview.className = "profile-row-preview";
      preview.textContent = profiles[name];
      const deleteBtn = iconButton("icon-btn icon-btn-danger", "Delete", TRASH_PATH);
      head.append(chevron, label, renameBtn, preview, deleteBtn);

      // Renaming turns the name into an editable field in place, rather than
      // a dialog — Enter/blur confirms, Escape cancels. A rename is really
      // create-new + carry the assignment + delete-old (there's no rename
      // route), since the store keys profiles by name.
      renameBtn.addEventListener("click", async (e) => {
        e.stopPropagation();
        const input = document.createElement("input");
        input.type = "text";
        input.className = "profile-row-rename-input";
        input.value = name;
        label.replaceWith(input);
        input.focus();
        input.select();

        let settled = false;
        const finish = async (commit) => {
          if (settled) return;
          settled = true;
          const newName = input.value.trim();
          if (!commit || !newName || newName === name) {
            input.replaceWith(label);
            return;
          }
          if (names.includes(newName)) {
            flashStatus(profilesStatusEl, `"${newName}" already exists`);
            input.replaceWith(label);
            return;
          }
          try {
            await renameProfile(name, newName, profiles[name]);
            await loadProfilesTab(true);
          } catch (err) {
            flashStatus(profilesStatusEl, "rename failed");
            input.replaceWith(label);
          }
        };
        input.addEventListener("keydown", (ke) => {
          if (ke.key === "Enter") finish(true);
          else if (ke.key === "Escape") finish(false);
        });
        input.addEventListener("blur", () => finish(true));
      });

      const detail = document.createElement("div");
      detail.className = "profile-row-detail";
      detail.hidden = true;

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
      detail.append(textarea, actions);

      // The name/preview row toggles the editor open — everywhere except the
      // rename and delete icon buttons, which have their own click handling.
      const isActionClick = (e) =>
        deleteBtn.contains(e.target) || renameBtn.contains(e.target) || e.target.tagName === "INPUT";
      const toggle = () => {
        const expanded = head.getAttribute("aria-expanded") === "true";
        head.setAttribute("aria-expanded", String(!expanded));
        detail.hidden = expanded;
      };
      head.addEventListener("click", (e) => {
        if (isActionClick(e)) return;
        toggle();
      });
      head.addEventListener("keydown", (e) => {
        if (isActionClick(e)) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          toggle();
        }
      });

      saveBtn.addEventListener("click", async () => {
        try {
          await putJson(`/api/profiles/${KIND}/definitions`, {
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
          deleteBtn.title = "Confirm delete?";
          deleteBtn.setAttribute("aria-label", "Confirm delete?");
          clearTimeout(deleteBtn._disarmTimer);
          deleteBtn._disarmTimer = setTimeout(() => {
            deleteBtn.dataset.armed = "false";
            deleteBtn.title = "Delete";
            deleteBtn.setAttribute("aria-label", "Delete");
          }, 3000);
          return;
        }
        clearTimeout(deleteBtn._disarmTimer);
        try {
          await putJson(`/api/profiles/${KIND}/definitions`, {
            name,
            text: null,
          });
          await loadProfilesTab(true);
        } catch (e) {
          flashStatus(profilesStatusEl, "delete failed");
        }
      });

      row.append(head, detail);
      profilesListEl.appendChild(row);
    }
  }

  // There's no rename route — the store keys a profile by name — so this is
  // create-new-with-the-old-text, carry any global/repo assignment pointing
  // at the old name over to the new one, then delete-old. Carrying the
  // assignment matters: without it a rename would silently fall back to the
  // built-in default wherever the old name was assigned, the same failure
  // mode `assigning_a_profile_name_that_does_not_exist_is_rejected` exists
  // to catch on the backend side.
  async function renameProfile(oldName, newName, text) {
    await putJson(`/api/profiles/${KIND}/definitions`, { name: newName, text });
    const p = await getJson(`/api/profiles/${KIND}`);
    if (p.global === oldName) {
      await putJson(`/api/profiles/${KIND}/global`, { profile: newName });
    }
    // `repos` is omitted from the response entirely when empty (canvas-core
    // skip_serializing_if), not sent as `{}` — Object.entries(undefined)
    // throws, which was silently aborting every rename with no repo
    // overrides before it ever reached the delete-old-name step.
    for (const [repo, assigned] of Object.entries(p.repos || {})) {
      if (assigned === oldName) {
        await putJson(`/api/profiles/${KIND}/repos`, { repo, profile: newName });
      }
    }
    await putJson(`/api/profiles/${KIND}/definitions`, { name: oldName, text: null });
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
    if (!force && loadProfilesTab._loaded && knownRepos.length) return;
    loadProfilesTab._loaded = true;

    await loadKnownRepos();

    try {
      const p = await getJson(`/api/profiles/${KIND}`);
      const names = Object.keys(p.profiles).sort();
      renderProfileRows(p.profiles, p.builtin);
      fillProfileSelect(profileGlobalSelectEl, names, p.global);
      const repo = profileRepoInputEl.value.trim();
      fillProfileSelect(profileRepoSelectEl, names, repo ? (p.repos || {})[repo] : "");
      profileRepoSelectEl.disabled = !repo;
    } catch (e) {
      flashStatus(profilesStatusEl, "could not reach canvasd");
    }
  }

  profileNewEl.addEventListener("click", () => {
    // Only one draft at a time — clicking + again just refocuses it.
    const existing = profilesListEl.querySelector(".profile-row-draft");
    if (existing) {
      existing.querySelector("input").focus();
      return;
    }

    const draft = document.createElement("div");
    draft.className = "profile-row profile-row-draft";

    const nameInput = document.createElement("input");
    nameInput.type = "text";
    nameInput.className = "guidance-repo-input";
    nameInput.placeholder = "Enter profile name";

    const textarea = document.createElement("textarea");
    textarea.className = "guidance-text";
    textarea.rows = 5;
    textarea.placeholder = "Write this profile's text…";

    const actions = document.createElement("div");
    actions.className = "guidance-actions";
    const saveBtn = document.createElement("button");
    saveBtn.type = "button";
    saveBtn.className = "btn";
    saveBtn.textContent = "Save";
    const cancelBtn = document.createElement("button");
    cancelBtn.type = "button";
    cancelBtn.className = "btn btn-secondary";
    cancelBtn.textContent = "Cancel";
    const status = document.createElement("span");
    status.className = "guidance-status";
    actions.append(saveBtn, cancelBtn, status);

    draft.append(nameInput, textarea, actions);
    profilesListEl.prepend(draft);
    nameInput.focus();

    cancelBtn.addEventListener("click", () => draft.remove());

    saveBtn.addEventListener("click", async () => {
      const name = nameInput.value.trim();
      if (!name) {
        flashStatus(status, "name required");
        return;
      }
      // Blank text is how the definitions route deletes a profile, so an
      // empty draft needs a real starter value to actually get created.
      const text = textarea.value.trim() || "(write this profile's text)";
      try {
        await putJson(`/api/profiles/${KIND}/definitions`, { name, text });
        await loadProfilesTab(true);
      } catch (e) {
        flashStatus(status, "add failed");
      }
    });
  });

  profileGlobalSelectEl.addEventListener("change", async () => {
    try {
      await putJson(`/api/profiles/${KIND}/global`, {
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
        `/api/profiles/${KIND}?repo=${encodeURIComponent(repo)}`
      );
      fillProfileSelect(profileRepoSelectEl, Object.keys(p.profiles).sort(), (p.repos || {})[repo]);
    } catch (e) {}
  }

  profileRepoInputEl.addEventListener("change", loadRepoAssignment);
  profileRepoInputEl.addEventListener("blur", loadRepoAssignment);

  profileRepoSelectEl.addEventListener("change", async () => {
    const repo = profileRepoInputEl.value.trim();
    if (!repo) return;
    try {
      await putJson(`/api/profiles/${KIND}/repos`, {
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
