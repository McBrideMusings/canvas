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

  for (const tab of tabs) {
    tab.addEventListener("click", () => {
      selectTab(tab.dataset.section);
      if (tab.dataset.section === "guidance") loadGuidanceTab();
    });
  }

  // --- Guidance tab: backed by canvasd, not localStorage — it's read by
  // every session's SessionStart hook, on this Mac, not just this browser
  // tab, so it has to live server-side.
  const guidanceGlobalEl = document.getElementById("guidance-global");
  const guidanceGlobalStatusEl = document.getElementById("guidance-global-status");
  const guidanceGlobalSaveEl = document.getElementById("guidance-global-save");
  const guidanceGlobalResetEl = document.getElementById("guidance-global-reset");
  const guidanceRepoInputEl = document.getElementById("guidance-repo-input");
  const guidanceRepoListEl = document.getElementById("guidance-repo-list");
  const guidanceRepoEl = document.getElementById("guidance-repo");
  const guidanceRepoSaveEl = document.getElementById("guidance-repo-save");
  const guidanceRepoResetEl = document.getElementById("guidance-repo-reset");
  const guidanceRepoStatusEl = document.getElementById("guidance-repo-status");

  let guidanceLoaded = false;

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

  async function loadGuidanceTab() {
    if (guidanceLoaded) return;
    guidanceLoaded = true;

    try {
      const state = await getJson("/api/state");
      const repos = Array.from(
        new Set(state.sessions.map((s) => s.repo).filter(Boolean))
      ).sort();
      guidanceRepoListEl.innerHTML = "";
      for (const repo of repos) {
        const option = document.createElement("option");
        option.value = repo;
        guidanceRepoListEl.appendChild(option);
      }
    } catch (e) {}

    try {
      const g = await getJson("/api/guidance");
      guidanceGlobalEl.value = g.global || "";
    } catch (e) {
      guidanceGlobalStatusEl.textContent = "could not reach canvasd";
    }
  }

  guidanceGlobalSaveEl.addEventListener("click", async () => {
    try {
      await putJson("/api/guidance/global", { text: guidanceGlobalEl.value });
      flashStatus(guidanceGlobalStatusEl, "saved");
    } catch (e) {
      flashStatus(guidanceGlobalStatusEl, "save failed");
    }
  });

  guidanceGlobalResetEl.addEventListener("click", async () => {
    guidanceGlobalEl.value = "";
    try {
      await putJson("/api/guidance/global", { text: null });
      flashStatus(guidanceGlobalStatusEl, "reset to default");
    } catch (e) {
      flashStatus(guidanceGlobalStatusEl, "reset failed");
    }
  });

  function setRepoControlsEnabled(enabled) {
    guidanceRepoEl.disabled = !enabled;
    guidanceRepoSaveEl.disabled = !enabled;
    guidanceRepoResetEl.disabled = !enabled;
  }

  async function loadRepoOverride() {
    const repo = guidanceRepoInputEl.value.trim();
    if (!repo) {
      guidanceRepoEl.value = "";
      setRepoControlsEnabled(false);
      return;
    }
    setRepoControlsEnabled(true);
    try {
      const g = await getJson(`/api/guidance?repo=${encodeURIComponent(repo)}`);
      guidanceRepoEl.value = g.repoOverride || "";
    } catch (e) {
      guidanceRepoEl.value = "";
    }
  }

  guidanceRepoInputEl.addEventListener("change", loadRepoOverride);
  guidanceRepoInputEl.addEventListener("blur", loadRepoOverride);

  guidanceRepoSaveEl.addEventListener("click", async () => {
    const repo = guidanceRepoInputEl.value.trim();
    if (!repo) return;
    try {
      await putJson("/api/guidance/repo", { repo, text: guidanceRepoEl.value });
      flashStatus(guidanceRepoStatusEl, "saved");
    } catch (e) {
      flashStatus(guidanceRepoStatusEl, "save failed");
    }
  });

  guidanceRepoResetEl.addEventListener("click", async () => {
    const repo = guidanceRepoInputEl.value.trim();
    if (!repo) return;
    guidanceRepoEl.value = "";
    try {
      await putJson("/api/guidance/repo", { repo, text: null });
      flashStatus(guidanceRepoStatusEl, "cleared");
    } catch (e) {
      flashStatus(guidanceRepoStatusEl, "clear failed");
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
})();
