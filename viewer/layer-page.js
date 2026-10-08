// Settings' Instructions and Post reminders tabs: one three-column page per
// kind. Column 1 holds the Include switch and the list of layers a person can
// edit (Yours, then each project that has posted to Canvas); column 2 edits
// the selected layer; column 3 shows what a session in the chosen project
// reads, composed by canvasd (POST /api/instructions/:kind/compose, the same
// `compose` the hooks call) from the unsaved text as it is typed. Saving is
// explicit per layer. canvasd's routes live in canvasd/src/instruction_routes.rs.
(function (root) {
  "use strict";

  const KINDS = {
    instructions: {
      noun: "instructions",
      file: ".canvas/instructions.md",
      markdown: true,
    },
    reminders: {
      noun: "reminders",
      file: ".canvas/reminders.txt",
      markdown: false,
    },
  };

  const DOWN = "Canvas's daemon isn't answering.";
  // Column 3 recomposes at most this often while typing.
  const COMPOSE_EVERY_MS = 100;
  const DOWN_RETRY_MS = 3000;

  // A request canvasd never answered (the app's bridge answers 503 when it
  // can't reach the socket), as opposed to one it refused.
  class DownError extends Error {}

  async function call(method, url, body) {
    let response;
    try {
      response = await fetch(url, {
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (e) {
      throw new DownError(DOWN);
    }
    if (response.status === 502 || response.status === 503 || response.status === 504) {
      throw new DownError(DOWN);
    }
    if (!response.ok) {
      const text = (await response.text()).trim();
      throw new Error(text || `HTTP ${response.status}`);
    }
    const type = response.headers.get("content-type") || "";
    return type.includes("json") ? response.json() : null;
  }

  function h(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function esc(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  // Characters as canvasd counts them (Rust `chars`): code points.
  const chars = (text) => Array.from(text).length;
  const lineCount = (text) => (text === "" ? 0 : text.split("\n").length);
  const fmt = (n) => n.toLocaleString("en-US");
  const plural = (n, word) => `${fmt(n)} ${word}${n === 1 ? "" : "s"}`;

  // One line of Markdown, coloured: headings, list markers, inline code,
  // bold and links. Escapes first, so the markup never reaches the DOM raw.
  function markdownLine(line) {
    if (/^#{1,6}(\s|$)/.test(line)) return `<span class="md-h">${esc(line)}</span>`;
    const marker = /^(\s*)([-*+]|\d+\.) /.exec(line);
    const rest = marker ? line.slice(marker[0].length) : line;
    // Inline code is split out first, so bold and links never match inside it.
    const html = rest
      .split(/(`[^`]+`)/)
      .map((part, i) => {
        if (i % 2) return `<span class="md-code">${esc(part)}</span>`;
        return esc(part)
          .replace(/\*\*([^*]+)\*\*/g, '<span class="md-b">**$1**</span>')
          .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<span class="md-link">[$1]($2)</span>');
      })
      .join("");
    return marker ? `${marker[1]}<span class="md-mark">${esc(marker[2])}</span> ${html}` : html;
  }

  // One reminders directive line: a `#` note is quieter than a directive.
  function directiveLine(line) {
    const t = esc(line);
    return /^\s*#/.test(line) ? `<span class="md-note">${t}</span>` : t;
  }

  // A path as the person reads it: their home folder as `~`, in a span the
  // `.lp-path` box clips from the start, so the end of the path stays visible.
  function pathEl(className, path) {
    const outer = h("span", className);
    outer.classList.add("lp-path");
    const inner = h("span", null, path.replace(/^\/Users\/[^/]+(?=\/|$)/, "~"));
    inner.dir = "ltr";
    outer.title = path;
    outer.append(inner);
    return outer;
  }

  function lineRow(n, html, extra) {
    return (
      `<div class="ln${extra ? " " + extra : ""}"><span class="ln-n">${n}</span>` +
      `<span class="ln-t">${html || "&#8203;"}</span></div>`
    );
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

  // The reminders form over `textarea`'s directive text. `layered()` says
  // whether a layer comes before this one, `base()` names what comes before.
  // Every edit rewrites the text and fires `input`, so the text stays the only
  // store. Returns the form element and a function that re-renders it.
  function reminderForm(textarea, layered, base) {
    const formEl = h("div", "stop-form");

    const edit = (fn) => {
      const model = StopForm.parse(textarea.value);
      fn(model);
      textarea.value = StopForm.emit(model);
      // The editor's input handler re-renders the form.
      textarea.dispatchEvent(new Event("input"));
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
    // layers above. First layer: there is nothing above, so On is a line and
    // Off is none.
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
          else render();
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
      const input = h("input", "lp-input");
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
        const removeBtn = h("button", "btn btn-secondary", "Remove from above");
        removeBtn.type = "button";
        removeBtn.addEventListener("click", () => apply(false));
        add.append(removeBtn);
      }
      block.append(chips, add);
      return block;
    }

    function render() {
      const layeredNow = layered();
      const model = StopForm.parse(textarea.value);
      formEl.innerHTML = "";
      formEl.append(
        h(
          "p",
          "stop-form-banner",
          layeredNow
            ? `These lines apply after ${base()}. Inherit leaves a setting as it is there; ` +
                "On and Off write a line that overrides it."
            : "Nothing comes before these lines, so anything not switched on is off."
        )
      );
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

    render();
    return { formEl, render };
  }

  // One kind's page, mounted once on its <main>.
  function Page(main) {
    const kind = main.dataset.kind;
    const spec = KINDS[kind];
    const includeEl = main.querySelector(".lp-include");
    const includeNoteEl = main.querySelector(".lp-include-note");
    const targetsEl = main.querySelector(".lp-targets");
    const editEl = main.querySelector(".lp-edit");
    const outEl = main.querySelector(".lp-out");

    // `person` and each project hold the saved text (null: no file) and the
    // draft being edited. `target` is "yours" or a project root; `project` is
    // the root column 3 composes for (null outside any project).
    const st = {
      loaded: false,
      down: false,
      include: true,
      person: { saved: "", draft: "" },
      projects: [],
      target: "yours",
      project: null,
      composed: null,
      composeError: "",
      view: "form",
      saving: false,
      saveError: "",
      savedFlash: false,
    };
    let retryTimer = null;

    const projectOf = (rootPath) => st.projects.find((p) => p.root === rootPath) || null;
    const current = () => (st.target === "yours" ? st.person : projectOf(st.target));
    const dirty = (layer) => layer && layer.saved !== null && layer.draft !== layer.saved;

    function markDown() {
      st.down = true;
      clearTimeout(retryTimer);
      retryTimer = setTimeout(() => {
        if (!main.hidden) load();
      }, DOWN_RETRY_MS);
    }

    let loadSeq = 0;

    async function load() {
      const seq = ++loadSeq;
      try {
        const [layers, projects] = await Promise.all([
          call("GET", `/api/instructions/${kind}`),
          call("GET", "/api/projects"),
        ]);
        if (seq !== loadSeq) return;
        const person = layers.layers.find((l) => l.source === "person");
        const personText = person ? person.text : "";
        // Keep a draft across a reload; refresh what's saved underneath it.
        if (!st.loaded || !dirty(st.person)) st.person.draft = personText;
        st.person.saved = personText;
        st.include = layers.include;
        const texts = await Promise.all(
          projects.map((p) =>
            p[kind]
              ? call("GET", `/api/instructions/${kind}?root=${encodeURIComponent(p.root)}`).then((v) => {
                  const layer = v.layers.find((l) => l.source === "project");
                  return layer ? layer.text : "";
                })
              : Promise.resolve(null)
          )
        );
        if (seq !== loadSeq) return;
        // Each project keeps its object across reloads, so a save or create
        // still waiting on canvasd lands on the project the page shows.
        st.projects = projects.map((p, i) => {
          const layer = projectOf(p.root) || { root: p.root, saved: null, draft: "" };
          const saved = texts[i];
          if (!dirty(layer)) layer.draft = saved === null ? "" : saved;
          layer.name = p.name;
          layer.saved = saved;
          return layer;
        });
        if (st.target !== "yours" && !projectOf(st.target)) st.target = "yours";
        if (!projectOf(st.project)) st.project = st.projects.length ? st.projects[0].root : null;
        st.loaded = true;
        st.down = false;
        clearTimeout(retryTimer);
        renderAll();
        compose();
      } catch (e) {
        if (!(e instanceof DownError)) throw e;
        if (seq !== loadSeq) return;
        markDown();
        renderAll();
      }
    }

    // --- Column 3: at most one compose in flight and one per 100ms; a
    // request that lands after a newer one started is dropped.
    let composeSeq = 0;
    let composeTimer = null;
    let composeLast = 0;
    let scrollToActive = true;

    function compose() {
      const wait = composeLast + COMPOSE_EVERY_MS - Date.now();
      clearTimeout(composeTimer);
      if (wait > 0) {
        composeTimer = setTimeout(compose, wait);
        return;
      }
      composeLast = Date.now();
      const seq = ++composeSeq;
      const project = projectOf(st.project);
      const body = {
        root: st.project,
        person: st.person.draft,
        project: project && project.saved !== null ? project.draft : null,
      };
      call("POST", `/api/instructions/${kind}/compose`, body).then(
        (composed) => {
          if (seq !== composeSeq) return;
          st.composed = composed;
          st.composeError = "";
          if (st.down) {
            st.down = false;
            renderEditor();
          }
          renderOut();
        },
        (e) => {
          if (seq !== composeSeq) return;
          if (e instanceof DownError) {
            markDown();
            renderEditor();
          } else st.composeError = e.message;
          renderOut();
        }
      );
    }

    // --- Column 1
    function renderNav() {
      includeEl.checked = st.include;
      includeEl.disabled = st.down;
      includeNoteEl.textContent = st.include
        ? "Sessions read them first, then yours, then the project's."
        : `Sessions read yours, then the project's. The built-in ${spec.noun} are not ${
            kind === "instructions" ? "sent" : "used"
          }.`;
      if (includeError) includeNoteEl.textContent = includeError;
      includeNoteEl.classList.toggle("lp-error", !!includeError);
      targetsEl.innerHTML = "";
      const row = (key, dotClass, showDot, name, meta, path) => {
        const btn = h("button", "lp-row");
        btn.type = "button";
        btn.dataset.target = key;
        btn.setAttribute("aria-current", String(st.target === key));
        const dot = h("span", `dot ${dotClass}`);
        if (!showDot) dot.classList.add("dot-hidden");
        btn.append(
          dot,
          h("span", "lp-row-name", name),
          h("span", "lp-row-meta", meta),
          key === "yours" ? h("span", "lp-row-path", path) : pathEl("lp-row-path", path)
        );
        return btn;
      };
      const meta = (layer) => {
        if (layer.saved === null) return "no file";
        const n = lineCount(layer.draft);
        return n === 0 ? "empty" : plural(n, "line");
      };
      targetsEl.append(row("yours", "dot-yours", true, "Yours", meta(st.person), "every session, every repo"));
      targetsEl.append(h("div", "lp-sub", "Projects"));
      if (!st.projects.length) {
        targetsEl.append(h("div", "lp-none", st.loaded ? "No project has posted to Canvas yet." : "—"));
      }
      for (const p of st.projects) {
        targetsEl.append(row(p.root, "dot-project", p.root === st.project, p.name, meta(p), p.root));
      }
    }

    targetsEl.addEventListener("click", (e) => {
      const btn = e.target.closest(".lp-row");
      if (!btn) return;
      st.target = btn.dataset.target;
      if (st.target !== "yours" && st.target !== st.project) {
        st.project = st.target;
        st.composed = null;
      }
      st.saveError = "";
      st.savedFlash = false;
      scrollToActive = true;
      renderAll();
      compose();
    });

    let includeError = "";

    includeEl.addEventListener("change", async () => {
      const include = includeEl.checked;
      includeError = "";
      try {
        await call("PUT", `/api/instructions/${kind}/include`, { include });
        st.include = include;
      } catch (e) {
        if (e instanceof DownError) markDown();
        else includeError = `Not changed: ${e.message}`;
      }
      renderNav();
      keepCaret(renderEditor);
      compose();
    });

    // --- Column 2
    function layerHead(dotClass, title, sub) {
      const head = h("div", "pane-head");
      head.append(
        h("span", `dot ${dotClass}`),
        h("span", "pane-title", title),
        sub.startsWith("/") ? pathEl("pane-sub", sub) : h("span", "pane-sub", sub)
      );
      return head;
    }

    function renderEditor() {
      editEl.innerHTML = "";
      const isYours = st.target === "yours";
      const layer = current();
      editEl._focus = null;
      const pane = h("div", "pane");
      editEl.append(pane);
      if (!layer) return;
      const title = isYours ? "Yours" : layer.name;
      const sub = isYours ? "every session, every repo" : `${layer.root}/${spec.file}`;
      const head = layerHead(isYours ? "dot-yours" : "dot-project", title, sub);
      pane.append(head);

      if (!st.loaded) {
        const body = h("div", "pane-body");
        const empty = h("div", "ed-empty");
        empty.append(h("b", null, `Can't load ${spec.noun}`), h("span", "lp-note", DOWN));
        body.append(empty);
        pane.append(body);
        return;
      }

      if (!isYours && layer.saved === null) {
        const body = h("div", "pane-body");
        const empty = h("div", "ed-empty");
        const note = h("span", "lp-note");
        note.append(
          "Creating them writes ",
          h("code", null, spec.file),
          " at the repo's git root. Commit it to share it with everyone who clones the repo."
        );
        const create = h("button", "btn lp-create", `Create ${spec.noun} for ${layer.name}`);
        create.type = "button";
        create.disabled = st.down || st.saving;
        if (st.down) create.title = DOWN;
        create.addEventListener("click", () => createProject(layer));
        empty.append(h("b", null, `No ${spec.noun} for ${layer.name} yet`), note, create);
        if (st.down) empty.append(h("span", "lp-error", `Can't create: ${DOWN}`));
        else if (st.saveError) empty.append(h("span", "lp-error", st.saveError));
        body.append(empty);
        pane.append(body);
        return;
      }

      const status = h("span", "pane-status");
      const save = h("button", "btn lp-save", st.saving ? "Saving…" : "Save");
      save.type = "button";
      head.append(status, save);

      const body = h("div", "pane-body");
      const wrap = h("div", "ed-wrap code");
      const hl = h("div", "ed-hl");
      hl.setAttribute("aria-hidden", "true");
      const ta = h("textarea", "ed-text");
      ta.spellcheck = false;
      ta.setAttribute("aria-label", `${title} ${spec.noun}`);
      ta.value = layer.draft;
      wrap.append(hl, ta);

      const foot = h("div", "pane-foot");
      const linesEl = h("span", "ed-lines");
      const charsEl = h("span", "ed-chars");
      foot.append(linesEl, charsEl);

      let form = null;
      if (kind === "reminders") {
        const bar = h("div", "stop-form-bar");
        const seg = h("span", "seg");
        seg.setAttribute("role", "group");
        seg.setAttribute("aria-label", "Editor view");
        const formBtn = h("button", "seg-btn", "Form");
        const textBtn = h("button", "seg-btn", "Text");
        formBtn.type = textBtn.type = "button";
        seg.append(formBtn, textBtn);
        bar.append(seg);
        form = reminderForm(
          ta,
          () => (isYours ? st.include : st.include || st.person.draft.trim() !== ""),
          () => {
            const above = [];
            if (st.include) above.push("the built-in reminders");
            if (!isYours && st.person.draft.trim()) above.push("yours");
            return above.join(" and ");
          }
        );
        const showView = (view) => {
          st.view = view;
          formBtn.setAttribute("aria-pressed", String(view === "form"));
          textBtn.setAttribute("aria-pressed", String(view === "text"));
          form.formEl.hidden = view !== "form";
          wrap.hidden = view !== "text";
          if (view === "form") form.render();
        };
        formBtn.addEventListener("click", () => showView("form"));
        textBtn.addEventListener("click", () => showView("text"));
        body.append(bar, form.formEl, wrap);
        showView(st.view);
      } else {
        body.append(wrap);
      }
      pane.append(body, foot);

      const paintStatus = () => {
        const unsaved = dirty(layer);
        save.disabled = st.down || st.saving || !unsaved;
        save.title = st.down ? DOWN : "";
        status.className = "pane-status";
        if (st.down) {
          status.classList.add("is-error");
          status.textContent = `Can't save: ${DOWN}`;
        } else if (st.saveError) {
          status.classList.add("is-error");
          status.textContent = st.saveError;
        } else if (unsaved) {
          status.classList.add("is-unsaved");
          status.textContent = "Unsaved";
        } else {
          status.classList.add("is-saved");
          status.textContent = "Saved";
        }
      };

      const paint = () => {
        const fn = spec.markdown ? markdownLine : directiveLine;
        hl.innerHTML = ta.value
          .split("\n")
          .map((l, i) => lineRow(i + 1, fn(l)))
          .join("");
        // As many lines as the gutter numbers.
        linesEl.textContent = plural(ta.value.split("\n").length, "line");
        charsEl.textContent = plural(chars(ta.value), "character");
        paintStatus();
      };

      ta.addEventListener("input", () => {
        layer.draft = ta.value;
        st.saveError = "";
        paint();
        renderNav();
        if (form && st.view === "form") form.render();
        compose();
      });
      ta.addEventListener("keydown", (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "s") {
          e.preventDefault();
          if (!save.disabled) saveLayer(layer);
        }
      });
      save.addEventListener("click", () => saveLayer(layer));
      paint();
      editEl._focus = () => {
        if (wrap.hidden) return;
        ta.focus();
        ta.setSelectionRange(ta.value.length, ta.value.length);
      };
    }

    async function saveLayer(layer) {
      const isYours = layer === st.person;
      const text = layer.draft;
      st.saving = true;
      st.saveError = "";
      renderAll();
      try {
        if (isYours) await call("PUT", `/api/instructions/${kind}/person`, { text });
        else await call("PUT", `/api/instructions/${kind}/project`, { root: layer.root, text });
        // A blank save deletes the file, so the project has none again.
        const blank = !text.trim();
        layer.saved = blank ? (isYours ? "" : null) : text;
        if (blank && layer.draft === text) layer.draft = "";
      } catch (e) {
        if (e instanceof DownError) markDown();
        else st.saveError = `Not saved: ${e.message}`;
      }
      st.saving = false;
      renderAll();
      compose();
    }

    // The empty state's button writes the file at once. canvasd deletes a
    // file saved blank, so it starts with one line naming the project: a
    // heading in instructions, a note in reminders, which the parser skips.
    async function createProject(layer) {
      const text = kind === "instructions" ? `# ${layer.name}` : `# ${layer.name}'s reminders`;
      st.saving = true;
      st.saveError = "";
      renderEditor();
      try {
        await call("PUT", `/api/instructions/${kind}/project`, { root: layer.root, text });
        layer.saved = text;
        layer.draft = text;
      } catch (e) {
        if (e instanceof DownError) markDown();
        else st.saveError = `Not created: ${e.message}`;
      }
      st.saving = false;
      renderAll();
      compose();
      if (layer.saved !== null && editEl._focus) editEl._focus();
    }

    // --- Column 3
    function renderOut() {
      const oldBody = outEl.querySelector(".pane-body");
      const keptScroll = oldBody ? oldBody.scrollTop : 0;
      outEl.innerHTML = "";
      const pane = h("div", "pane");
      const project = projectOf(st.project);
      const head = h("div", "pane-head");
      head.append(
        h("span", "pane-title", project ? `What a ${project.name} session reads` : "What a session reads"),
        h("span", "pane-sub", ""),
        h("span", "pane-badge", "updates as you type")
      );
      pane.append(head);
      const body = h("div", "pane-body");
      const out = h("div", "out-body code");
      out.setAttribute("data-composed", "");
      body.append(out);
      const foot = h("div", "pane-foot");
      pane.append(body, foot);
      outEl.append(pane);

      if (st.down) {
        const empty = h("div", "ed-empty");
        empty.append(h("span", "lp-unavailable", `Preview unavailable: ${DOWN}`));
        out.replaceWith(empty);
        return;
      }
      if (st.composeError) {
        const empty = h("div", "ed-empty");
        empty.append(h("span", "lp-error", `Preview unavailable: ${st.composeError}`));
        out.replaceWith(empty);
        return;
      }
      const composed = st.composed;
      if (!composed) return;
      out._text = composed.text;

      const activeSource = st.target === "yours" ? "person" : "project";
      const fn = spec.markdown ? markdownLine : directiveLine;
      const textLines = composed.text === "" ? [] : composed.text.split("\n");
      let html = "";
      let n = 1;
      for (const layer of composed.layers) {
        // A blank line between layers belongs to neither.
        while (n < layer.startLine) {
          html += lineRow(n, "", "out-blank");
          n += 1;
        }
        const dot = { "built-in": "dot-builtin", person: "dot-yours", project: "dot-project" }[layer.source];
        html += `<div class="out-block${layer.source === activeSource ? " is-active" : ""}" data-source="${layer.source}">`;
        html += `<div class="out-break"><span class="dot ${dot}"></span><span class="out-break-label">${esc(layer.name)}</span></div>`;
        for (let i = 0; i < layer.lines; i += 1, n += 1) html += lineRow(n, fn(textLines[n - 1] || ""));
        html += "</div>";
      }
      out.innerHTML = html;
      if (!composed.layers.length) {
        out.append(h("div", "ed-empty lp-note", `A session here reads no ${spec.noun}.`));
      }

      foot.append(
        // The composed text ends with one newline, which starts no line.
        h("span", null, plural(lineCount(composed.text.replace(/\n$/, "")), "line")),
        h("span", null, plural(chars(composed.text), "character")),
        h("span", "grow")
      );
      const legend = h("span", "out-legend");
      for (const layer of composed.layers) {
        const dot = { "built-in": "dot-builtin", person: "dot-yours", project: "dot-project" }[layer.source];
        const item = h("span", "out-legend-item");
        item.append(h("span", `dot ${dot}`), layer.name);
        legend.append(item);
      }
      if (project && !composed.layers.some((l) => l.source === "project")) {
        legend.append(h("span", "out-legend-item out-legend-none", `${project.name}: none`));
      }
      foot.append(legend);

      // A recompose keeps the reader's place; choosing a layer brings its
      // block into view.
      if (scrollToActive) {
        scrollToActive = false;
        const active = out.querySelector(".is-active");
        body.scrollTop = active ? active.offsetTop - 8 : 0;
      } else body.scrollTop = keptScroll;
    }

    // Rebuilding the editor keeps the caret where the person left it.
    function keepCaret(fn) {
      const ta = editEl.querySelector(".ed-text");
      const focused = ta && document.activeElement === ta;
      const [start, end] = ta ? [ta.selectionStart, ta.selectionEnd] : [0, 0];
      fn();
      const next = editEl.querySelector(".ed-text");
      if (focused && next && !next.closest("[hidden]")) {
        next.focus();
        next.setSelectionRange(Math.min(start, next.value.length), Math.min(end, next.value.length));
      }
    }

    function renderAll() {
      renderNav();
      keepCaret(renderEditor);
      renderOut();
    }

    renderAll();
    return {
      open() {
        scrollToActive = true;
        load();
      },
      // For `admin verify-app eval`: what column 3 shows, as text.
      composedText: () => (st.composed ? st.composed.text : null),
    };
  }

  const pages = {};
  root.LayerPage = {
    open(kind) {
      if (!pages[kind]) pages[kind] = Page(document.getElementById(`section-${kind}`));
      pages[kind].open();
    },
    page: (kind) => pages[kind] || null,
  };
})(window);
