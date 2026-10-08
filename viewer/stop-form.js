// The stop-triggers form's model: a view over the profile's directive text
// (parsed by canvas-core/src/reminders.rs). Text stays the only store — the
// model keeps every line, so comments, blank lines, ordering and lines the
// form doesn't understand survive an edit. Later lines override earlier
// ones, so a change rewrites a key's last line in place, never reorders.
(function (root) {
  const DEFAULT_LINKS = 3;
  const DEFAULT_LONG_BLOCK = 15;
  const FLAGS = ["image", "file", "report", "verify"];

  // One line -> { raw, key, value } for a directive the daemon accepts, or
  // { raw, key: null } for a comment, blank line or anything else. `key` names
  // what the line sets: a flag, "links", "long-block", "enabled",
  // "phrase:<lower>" or "scratch:<prefix>"; `value` is true, false or a number.
  function parseLine(raw) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) return { raw, key: null };
    let negated = false;
    let rest = line;
    if (line.startsWith("no ")) {
      negated = true;
      rest = line.slice(3).trim();
    }
    const m = /^(\S+)(?:\s+(.*))?$/.exec(rest);
    if (!m) return { raw, key: null };
    const word = m[1];
    const arg = (m[2] || "").trim();
    const on = !negated;
    if (!negated && arg === "" && (word === "off" || word === "on")) {
      return { raw, key: "enabled", value: word === "on" };
    }
    if (FLAGS.includes(word) && arg === "") return { raw, key: word, value: on };
    if (word === "links" || word === "long-block") {
      if (negated) return arg === "" ? { raw, key: word, value: false } : { raw, key: null };
      const fallback = word === "links" ? DEFAULT_LINKS : DEFAULT_LONG_BLOCK;
      if (arg === "") return { raw, key: word, value: fallback };
      // Rust's usize parse accepts a leading `+`; a value past 2^53 is left as an
      // unread line, which the daemon still accepts and the form keeps as written.
      const n = /^\+?\d+$/.test(arg) ? Number(arg) : NaN;
      return Number.isSafeInteger(n) ? { raw, key: word, value: n } : { raw, key: null };
    }
    if ((word === "phrase" || word === "scratch") && arg !== "") {
      return {
        raw,
        key: word === "phrase" ? "phrase:" + arg.toLowerCase() : "scratch:" + arg,
        value: on,
        label: word === "phrase" ? arg.toLowerCase() : arg,
      };
    }
    return { raw, key: null };
  }

  function parse(text) {
    const lines = text === "" ? [] : text.replace(/\n$/, "").split("\n").map(parseLine);
    return { lines };
  }

  function emit(model) {
    return model.lines.length ? model.lines.map((l) => l.raw).join("\n") + "\n" : "";
  }

  // The last line setting `key` wins, exactly as in the daemon's parser.
  function get(model, key) {
    let found;
    for (const l of model.lines) if (l.key === key) found = l.value;
    return found;
  }

  function lineFor(key, value, label) {
    if (key === "enabled") return value ? "on" : "off";
    if (key === "links" || key === "long-block") {
      return value === false ? "no " + key : `${key} ${value}`;
    }
    if (key.startsWith("phrase:")) return (value ? "phrase " : "no phrase ") + label;
    if (key.startsWith("scratch:")) return (value ? "scratch " : "no scratch ") + label;
    return value ? key : "no " + key;
  }

  // Sets `key` to `value`, or clears it when `value` is undefined (no line
  // says anything about it). The key's last line is rewritten where it
  // stands and its earlier lines drop; an unset key's line goes at the end.
  function set(model, key, value, label) {
    const at = [];
    model.lines.forEach((l, i) => {
      if (l.key === key) at.push(i);
    });
    if (value === undefined) {
      model.lines = model.lines.filter((l) => l.key !== key);
      return;
    }
    const line = parseLine(lineFor(key, value, label));
    if (!at.length) {
      model.lines.push(line);
      return;
    }
    const last = at[at.length - 1];
    model.lines[last] = line;
    model.lines = model.lines.filter((l, i) => l.key !== key || i === last);
  }

  // Phrase or scratch entries in first-mention order, with what each ends as.
  function entries(model, prefix) {
    const seen = new Map();
    for (const l of model.lines) {
      if (l.key && l.key.startsWith(prefix)) seen.set(l.key, { key: l.key, label: l.label, value: l.value });
    }
    return [...seen.values()];
  }

  // Lines that are neither blank, a comment, nor a directive the form reads.
  function unknownLines(model) {
    return model.lines.filter((l) => l.key === null && l.raw.trim() && !l.raw.trim().startsWith("#"));
  }

  const api = { parse, emit, get, set, entries, unknownLines, DEFAULT_LINKS, DEFAULT_LONG_BLOCK };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.StopForm = api;
})(typeof window !== "undefined" ? window : globalThis);
