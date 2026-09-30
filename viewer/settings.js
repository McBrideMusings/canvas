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
  // (canvasd/src/profiles.rs) holds one set of named profiles per "kind";
  // the switcher at the top picks which kind every control below edits.
  const KINDS = {
    "posting-guidance": {
      file: "plugin/guidance.md",
      note:
        "The text a session reads when it starts. It tells the agent when to post to Canvas.",
      placeholder:
        "Text added after the global profile (or built-in default) for sessions in a repo that uses this profile…",
    },
    "stop-triggers": {
      file: "plugin/stop-triggers.txt",
      note:
        "Which turns the prompt hook reminds an agent to post, on the prompt after the turn. One directive per line, " +
        "top to bottom, later lines override earlier ones: image, file, report, verify, links [N], " +
        "long-block [N], phrase <text>, scratch <prefix>, no <directive>, off, on. " +
        "Lines starting with # are notes. Open the built-in default below to see the full list.",
      placeholder:
        "Directives added after the global profile (or built-in default), e.g. no links, long-block 30, off",
    },
  };
  let KIND = "posting-guidance";
  const kindButtons = document.querySelectorAll("[data-profile-kind]");
  const profileKindNoteEl = document.getElementById("profile-kind-note");
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
  const profileModeButtons = document.querySelectorAll("[data-profile-mode]");
  const profileModeNoteEl = document.getElementById("profile-mode-note");
  const profileModeStatusEl = document.getElementById("profile-mode-status");
  const profileRepoEffectiveEl = document.getElementById("profile-repo-effective");

  let builtinText = null;
  let profileMode = "additive";

  const MODE_NOTES = {
    additive:
      "A session gets the global profile (or the built-in default when none is assigned), " +
      "then its repo's profile after it.",
    replace:
      "A session gets its repo's profile alone, or the global profile when its repo has none.",
  };

  function renderMode(mode) {
    profileMode = mode;
    for (const btn of profileModeButtons) {
      btn.setAttribute("aria-pressed", String(btn.dataset.profileMode === mode));
    }
    profileModeNoteEl.textContent = MODE_NOTES[mode];
    refreshForms();
  }

  // What a session in the typed repo receives, per the daemon's own join.
  let effectiveSeq = 0;
  async function renderRepoEffective() {
    const seq = ++effectiveSeq;
    const repo = profileRepoInputEl.value.trim();
    if (!repo) {
      profileRepoEffectiveEl.textContent = "";
      return;
    }
    try {
      const e = await getJson(
        `/api/profiles/${KIND}/effective?repo=${encodeURIComponent(repo)}`
      );
      if (seq !== effectiveSeq) return;
      const sources = e.profiles || [];
      profileRepoEffectiveEl.textContent = sources.length
        ? `Sessions in ${repo} receive: ${sources.join(" + ")}`
        : `Sessions in ${repo} receive: built-in default (nothing assigned)`;
    } catch (err) {
      profileRepoEffectiveEl.textContent = "";
    }
  }

  for (const btn of profileModeButtons) {
    btn.addEventListener("click", async () => {
      const mode = btn.dataset.profileMode;
      if (mode === profileMode) return;
      try {
        await putJson(`/api/profiles/${KIND}/mode`, { mode });
        renderMode(mode);
        flashStatus(profileModeStatusEl, "saved");
        renderRepoEffective();
      } catch (e) {
        flashStatus(profileModeStatusEl, "save failed");
      }
    });
  }

  async function getJson(url) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return response.json();
  }

  // A 400 carries the daemon's reason (which directive line is invalid), kept
  // on `detail` so a save can show it instead of a bare "save failed".
  async function putJson(url, body) {
    const response = await fetch(url, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      const err = new Error(`${url}: HTTP ${response.status}`);
      err.detail = response.status === 400 ? (await response.text()).trim() : "";
      throw err;
    }
  }

  function flashStatus(el, text, ms = 2000) {
    el.textContent = text;
    clearTimeout(el._timer);
    el._timer = setTimeout(() => {
      el.textContent = "";
    }, ms);
  }

  // A save failure, with the daemon's reason when it gave one.
  function flashFailure(el, err, fallback) {
    flashStatus(el, err && err.detail ? err.detail : fallback, err && err.detail ? 8000 : 2000);
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

  // --- Post-reminder triggers form: a view over the profile text (model in
  // stop-form.js). The textarea stays the store; every control reparses it,
  // edits one directive and writes the text back, so the raw view, the form
  // and `canvas profile` always agree.
  let globalName = null;
  let formRefreshers = [];

  // Additive mode applies a repo profile after the global one, so any profile
  // that isn't the global assignment is a set of edits to a base.
  const isLayered = (name) => profileMode === "additive" && name !== globalName;
  const layerBase = () => (globalName ? `global profile "${globalName}"` : "built-in default");

  function h(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  const FORM_FLAGS = [
    ["image", "Image", "An image was looked at"],
    ["file", "File", "A file changed outside scratch space"],
    ["report", "Report", "The turn ends with a closing report"],
    ["verify", "Verify", "The user is asked to verify or try something"],
  ];
  const FORM_COUNTS = [
    ["links", "Links", "or more links in a reply", StopForm.DEFAULT_LINKS],
    ["long-block", "Long block", "or more lines in a code block or table", StopForm.DEFAULT_LONG_BLOCK],
  ];

  // Builds the Form/Text switch and the form for `textarea`; the caller
  // appends the returned nodes just before it. `layered()` says whether this profile is applied after a base, `base()`
  // names that base.
  function stopFormNodes(textarea, layered, base) {
    const formEl = h("div", "stop-form");
    const switcher = h("span", "seg");
    switcher.setAttribute("role", "group");
    switcher.setAttribute("aria-label", "Editor view");
    const formBtn = h("button", "seg-btn", "Form");
    const textBtn = h("button", "seg-btn", "Text");
    formBtn.type = textBtn.type = "button";
    switcher.append(formBtn, textBtn);
    const bar = h("div", "stop-form-bar");
    bar.append(switcher);

    let view = "form";
    const showView = (next) => {
      view = next;
      formBtn.setAttribute("aria-pressed", String(view === "form"));
      textBtn.setAttribute("aria-pressed", String(view === "text"));
      formEl.hidden = view !== "form";
      textarea.hidden = view !== "text";
      if (view === "form") renderForm();
    };
    formBtn.addEventListener("click", () => showView("form"));
    textBtn.addEventListener("click", () => showView("text"));

    const edit = (fn) => {
      const model = StopForm.parse(textarea.value);
      fn(model);
      textarea.value = StopForm.emit(model);
      renderForm();
    };

    function segControl(label, states, current, onPick) {
      const seg = h("span", "seg");
      seg.setAttribute("role", "group");
      seg.setAttribute("aria-label", label);
      for (const [value, text] of states) {
        const btn = h("button", "seg-btn", text);
        btn.type = "button";
        btn.setAttribute("aria-pressed", String(value === current));
        btn.addEventListener("click", () => onPick(value));
        seg.append(btn);
      }
      return seg;
    }

    // Layered: Inherit writes no line, On/Off write one that overrides the
    // base. Standalone: there is no base, so On is a line and Off is none.
    function stateOf(value, isLayered) {
      if (value === undefined) return isLayered ? "inherit" : "off";
      return value === false ? "off" : "on";
    }

    function renderRow(model, layeredNow, key, label, hint, count) {
      const row = h("div", "stop-form-row");
      const name = h("div", "stop-form-name");
      name.append(h("span", "stop-form-label", label), h("span", "stop-form-hint", hint));
      const value = StopForm.get(model, key);
      const state = stateOf(value, layeredNow);
      const states = layeredNow
        ? [["inherit", "Inherit"], ["on", "On"], ["off", "Off"]]
        : [["on", "On"], ["off", "Off"]];
      const control = segControl(label, states, state, (next) => {
        edit((m) => {
          if (next === "inherit" || (next === "off" && !layeredNow)) StopForm.set(m, key, undefined);
          else if (next === "off") StopForm.set(m, key, false);
          else StopForm.set(m, key, count ? count[3] : true);
        });
      });
      const end = h("span", "stop-form-end");
      if (count) {
        const input = h("input", "stop-form-number");
        input.type = "number";
        input.min = "1";
        input.step = "1";
        input.setAttribute("aria-label", `${label} threshold`);
        input.disabled = state !== "on";
        input.value = typeof value === "number" ? String(value) : "";
        input.placeholder = String(count[3]);
        input.addEventListener("change", () => {
          const n = Number.parseInt(input.value, 10);
          if (n >= 1) edit((m) => StopForm.set(m, key, n));
          else renderForm();
        });
        end.append(input, h("span", "stop-form-hint", count[2]));
      }
      row.append(name, control, end);
      return row;
    }

    function renderList(model, layeredNow, prefix, title, hint, word) {
      const block = h("div", "stop-form-list");
      block.append(h("div", "stop-form-label", title), h("div", "stop-form-hint", hint));
      const chips = h("div", "stop-form-chips");
      for (const entry of StopForm.entries(model, prefix)) {
        if (entry.value === false && !layeredNow) continue;
        const chip = h("span", "stop-chip" + (entry.value ? "" : " stop-chip-removed"));
        chip.append(h("span", "stop-chip-text", (entry.value ? "" : "no ") + entry.label));
        const x = h("button", "stop-chip-x", "×");
        x.type = "button";
        x.title = "Remove this line";
        x.setAttribute("aria-label", `Remove ${entry.value ? "" : "no "}${entry.label}`);
        x.addEventListener("click", () => edit((m) => StopForm.set(m, entry.key, undefined)));
        chip.append(x);
        chips.append(chip);
      }
      const add = h("div", "stop-form-add");
      const input = h("input", "guidance-repo-input");
      input.type = "text";
      input.placeholder = word === "phrase" ? "phrase to count as asking to verify" : "path prefix, e.g. /tmp/";
      input.setAttribute("aria-label", `New ${word}`);
      const apply = (value) => {
        const text = input.value.trim();
        if (!text) return;
        const label = word === "phrase" ? text.toLowerCase() : text;
        edit((m) => StopForm.set(m, `${word}:${label}`, value, label));
      };
      const addBtn = h("button", "btn btn-secondary", "Add");
      addBtn.type = "button";
      addBtn.addEventListener("click", () => apply(true));
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          apply(true);
        }
      });
      add.append(input, addBtn);
      if (layeredNow) {
        const removeBtn = h("button", "btn btn-secondary", "Remove from base");
        removeBtn.type = "button";
        removeBtn.addEventListener("click", () => apply(false));
        add.append(removeBtn);
      }
      block.append(chips, add);
      return block;
    }

    function renderForm() {
      const layeredNow = layered();
      const model = StopForm.parse(textarea.value);
      formEl.innerHTML = "";
      const banner = h("p", "stop-form-banner");
      banner.textContent = layeredNow
        ? `Layered: these lines are applied after the ${base()}. Inherit leaves a setting as it is; ` +
          "On and Off write a line that overrides it."
        : "Standalone: this profile is the whole configuration, so anything not switched on is off.";
      formEl.append(banner);
      for (const [key, label, hint] of FORM_FLAGS) {
        formEl.append(renderRow(model, layeredNow, key, label, hint, null));
      }
      for (const count of FORM_COUNTS) {
        formEl.append(renderRow(model, layeredNow, count[0], count[1], "", count));
      }
      formEl.append(
        renderList(model, layeredNow, "phrase:", "Phrases", "Text that counts as asking the user to verify", "phrase"),
        renderList(model, layeredNow, "scratch:", "Scratch prefixes", "Files under these paths are not worth posting", "scratch")
      );
      // The hook's own switch: standalone shows the hook as on unless an
      // `off` line says otherwise.
      const enabled = StopForm.get(model, "enabled");
      const row = h("div", "stop-form-row");
      const name = h("div", "stop-form-name");
      name.append(h("span", "stop-form-label", "Reminder"), h("span", "stop-form-hint", "Off stops every trigger above"));
      const states = layeredNow
        ? [["inherit", "Inherit"], ["on", "On"], ["off", "Off"]]
        : [["on", "On"], ["off", "Off"]];
      const state = enabled === undefined ? (layeredNow ? "inherit" : "on") : enabled ? "on" : "off";
      row.append(
        name,
        segControl("Reminder", states, state, (next) =>
          edit((m) => {
            if (next === "inherit" || (next === "on" && !layeredNow)) StopForm.set(m, "enabled", undefined);
            else StopForm.set(m, "enabled", next === "on");
          })
        )
      );
      formEl.append(row);
      const unknown = StopForm.unknownLines(model);
      if (unknown.length) {
        formEl.append(
          h(
            "p",
            "stop-form-banner",
            `${unknown.length} line${unknown.length === 1 ? "" : "s"} the form doesn't read (kept as written): ` +
              unknown.map((l) => l.raw.trim()).join(" · ")
          )
        );
      }
    }

    formRefreshers.push(() => {
      if (formEl.isConnected && view === "form") renderForm();
    });
    showView("form");
    return [bar, formEl];
  }

  function refreshForms() {
    for (const fn of formRefreshers) fn();
  }

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
    note.textContent = `Compiled into this build of Canvas — edit ${KINDS[KIND].file} to change it.`;
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
    formRefreshers = [];
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
      const stopForm =
        KIND === "stop-triggers" ? stopFormNodes(textarea, () => isLayered(name), layerBase) : [];

      const actions = document.createElement("div");
      actions.className = "guidance-actions";
      const saveBtn = document.createElement("button");
      saveBtn.type = "button";
      saveBtn.className = "btn";
      saveBtn.textContent = "Save";
      const status = document.createElement("span");
      status.className = "guidance-status";
      actions.append(saveBtn, status);
      detail.append(...stopForm, textarea, actions);

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
          flashFailure(status, e, "save failed");
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
      builtinText = p.builtin || null;
      globalName = p.global || null;
      renderMode(p.mode || "additive");
      const names = Object.keys(p.profiles).sort();
      renderProfileRows(p.profiles, p.builtin);
      fillProfileSelect(profileGlobalSelectEl, names, p.global);
      const repo = profileRepoInputEl.value.trim();
      fillProfileSelect(profileRepoSelectEl, names, repo ? (p.repos || {})[repo] : "");
      profileRepoSelectEl.disabled = !repo;
      renderRepoEffective();
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
    // Replace mode sends a profile alone, so it starts from the built-in
    // text to edit. Additive mode appends it to the base, so it starts blank.
    if (profileMode === "replace" && builtinText) {
      textarea.value = builtinText;
    } else {
      textarea.placeholder = KINDS[KIND].placeholder;
    }
    // A draft is layered in additive mode: it starts blank and is appended to
    // a base, and nothing global can point at it before it is saved.
    const stopForm =
      KIND === "stop-triggers" ? stopFormNodes(textarea, () => isLayered(null), layerBase) : [];

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

    draft.append(nameInput, ...stopForm, textarea, actions);
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
      const text =
        textarea.value.trim() ||
        (profileMode === "replace" && builtinText) ||
        "(write this profile's text)";
      try {
        await putJson(`/api/profiles/${KIND}/definitions`, { name, text });
        await loadProfilesTab(true);
      } catch (e) {
        flashFailure(status, e, "add failed");
      }
    });
  });

  function renderKind() {
    for (const btn of kindButtons) {
      btn.setAttribute("aria-pressed", String(btn.dataset.profileKind === KIND));
    }
    profileKindNoteEl.textContent = KINDS[KIND].note;
  }

  for (const btn of kindButtons) {
    btn.addEventListener("click", () => {
      if (btn.dataset.profileKind === KIND) return;
      KIND = btn.dataset.profileKind;
      renderKind();
      loadProfilesTab(true);
    });
  }
  renderKind();

  profileGlobalSelectEl.addEventListener("change", async () => {
    try {
      await putJson(`/api/profiles/${KIND}/global`, {
        profile: profileGlobalSelectEl.value || null,
      });
      globalName = profileGlobalSelectEl.value || null;
      refreshForms();
      flashStatus(profileGlobalStatusEl, "saved");
      renderRepoEffective();
    } catch (e) {
      flashStatus(profileGlobalStatusEl, "save failed");
    }
  });

  async function loadRepoAssignment() {
    const repo = profileRepoInputEl.value.trim();
    if (!repo) {
      profileRepoSelectEl.disabled = true;
      renderRepoEffective();
      return;
    }
    profileRepoSelectEl.disabled = false;
    try {
      const p = await getJson(`/api/profiles/${KIND}`);
      fillProfileSelect(profileRepoSelectEl, Object.keys(p.profiles).sort(), (p.repos || {})[repo]);
    } catch (e) {}
    renderRepoEffective();
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
      renderRepoEffective();
    } catch (e) {
      flashStatus(profileRepoStatusEl, "save failed");
    }
  });

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

  // A URL hash (`#profiles`) opens straight to that section instead of the
  // General default — the gear icon per section, and a way to link/script a
  // specific settings pane without clicking through the tab bar. Runs last
  // so every element and function this section touches is already defined.
  if (sections.has(location.hash.slice(1))) {
    openTab(location.hash.slice(1));
  }
})();
