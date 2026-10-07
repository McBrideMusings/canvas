/* Served first in every artifact HTML page (artifacts::served_page), joined
   onto one line so the page's own line numbers don't move: so no line
   comments, and every statement ends in a semicolon. It posts each uncaught
   error and unhandled rejection to the viewer as canvas-artifact-error, and
   a click on an http(s) link as canvas-artifact-open, so the viewer opens it
   in the default browser instead of the pane navigating away. It also
   reports the page's address and applies a change to the artifact's files
   without the viewer reloading the pane (see canvas-artifact-changed below).

   The pane's opaque origin makes WebKit report nearly every error to window
   as a bare "Script error.": anything but the top level of a script fetched
   with CORS. So the relay wraps the entry points the browser calls back
   into (timers, animation frames, microtasks, event listeners, on* handler
   properties, observers): a wrapped callback that throws reports the real
   error, from its own stack, then rethrows it unchanged, and the masked
   copy that reaches window is dropped. */
(() => {
  const send = (kind, message, where) => {
    try {
      parent.postMessage({
        type: "canvas-artifact-error",
        kind,
        message: String(message),
        source: where[0] || undefined,
        line: where[1] || undefined,
        column: where[2] || undefined,
      }, "*");
    } catch (_) {}
  };
  /* WebKit stack frames read "name@url:line:column"; the first frame with a
     location is where it was thrown. */
  const where = (error) => {
    const frames = String((error && error.stack) || "").split("\n");
    for (const frame of frames) {
      const m = /^(?:[^@]*@)?(.+):(\d+):(\d+)$/.exec(frame);
      if (m) return [m[1], +m[2], +m[3]];
    }
    return [];
  };
  const describe = (e) => e instanceof Error ? `${e.name}: ${e.message}` : String(e);
  let rethrown = 0;
  const listen = EventTarget.prototype.addEventListener;
  listen.call(window, "error", (e) => {
    if (rethrown > 0) {
      rethrown--;
      return;
    }
    if (e.message !== "Script error." || e.filename) {
      send("error", e.message || describe(e.error), [e.filename, e.lineno, e.colno]);
      return;
    }
    const script = document.currentScript;
    const start = script && !script.src
      ? ` (the one starting "${script.text.trim().slice(0, 60).replace(/\s+/g, " ")}")`
      : "";
    send("error", script
      ? `Script error. WebKit hides the message and line of an error thrown at the top level of an inline <script>${start}; move that script into a .js file to see them`
      : "Script error. WebKit hid this error's message and line; it came from code canvas does not wrap, such as an inline on…= attribute", []);
  });
  listen.call(window, "unhandledrejection", (e) => send("rejection", describe(e.reason), where(e.reason)));

  /* The browser reports a rethrown error before its next task, so a count
     left over by then belongs to nothing. */
  const later = window.setTimeout;
  const forgetRethrown = () => later.call(window, () => { rethrown = 0; }, 0);
  const guards = new WeakMap();
  const originals = new WeakMap();
  const guard = (fn) => {
    if (typeof fn !== "function") return fn;
    if (originals.has(fn)) return fn;
    let guarded = guards.get(fn);
    if (!guarded) {
      guarded = function () {
        try {
          return fn.apply(this, arguments);
        } catch (error) {
          send("error", describe(error), where(error));
          rethrown++;
          forgetRethrown();
          throw error;
        }
      };
      guards.set(fn, guarded);
      originals.set(guarded, fn);
    }
    return guarded;
  };
  listen.call(document, "click", (e) => {
    if (e.defaultPrevented) return;
    const a = e.target && e.target.closest && e.target.closest("a[href]");
    const href = a && a.href;
    const url = typeof href === "string" ? href : href && typeof href.baseVal === "string" ? new URL(href.baseVal, document.baseURI).href : "";
    if (!/^https?:\/\//i.test(url)) return;
    e.preventDefault();
    try {
      parent.postMessage({ type: "canvas-artifact-open", url }, "*");
    } catch (_) {}
  });
  /* The page's own address goes up as it starts (start: the relay is now
     listening for changes) and on every navigation inside it, so the viewer
     reopens the artifact where it was left; pagehide tells the viewer the
     relay has stopped listening. */
  const up = (message) => {
    try {
      parent.postMessage(message, "*");
    } catch (_) {}
  };
  const report = () => up({ type: "canvas-artifact-location", href: location.href });
  up({ type: "canvas-artifact-location", href: location.href, start: true });
  listen.call(window, "hashchange", report);
  listen.call(window, "popstate", report);
  listen.call(window, "pagehide", () => up({ type: "canvas-artifact-unload" }));
  try {
    for (const name of ["pushState", "replaceState"]) {
      const original = history[name];
      if (typeof original === "function") {
        history[name] = function () {
          const result = original.apply(this, arguments);
          report();
          return result;
        };
      }
    }
  } catch (_) {}

  /* When the artifact's files change the viewer posts canvas-artifact-changed
     {stamp, paths} (paths null when it can't name them), acknowledged on
     receipt with canvas-artifact-ack {stamp}, instead of reloading the pane. The
     page sees it as a cancelable canvas-artifact-changed event on window whose
     detail is {paths}; a page that calls preventDefault() applies the change
     itself and keeps its state. Otherwise a change only to stylesheets the
     page links is swapped in place, and anything else reloads the page at
     the address it shows, hash and all. */
  const base = (/^\/artifacts\/[^/]+\//.exec(location.pathname) || [""])[0];
  const swapStyles = (paths) => {
    const swapped = new Set();
    for (const link of document.querySelectorAll('link[rel~="stylesheet"][href]')) {
      let url;
      try {
        url = new URL(link.href);
      } catch (_) {
        continue;
      }
      if (!base || url.protocol !== location.protocol || url.host !== location.host || !url.pathname.startsWith(base)) continue;
      let path;
      try {
        path = decodeURIComponent(url.pathname.slice(base.length));
      } catch (_) {
        continue;
      }
      if (!paths.includes(path)) continue;
      swapped.add(path);
      url.searchParams.set("canvas-reload", String(Date.now()));
      const next = link.cloneNode();
      next.href = url.href;
      listen.call(next, "load", () => link.remove());
      listen.call(next, "error", () => location.reload());
      link.after(next);
    }
    return paths.every((path) => swapped.has(path));
  };
  listen.call(window, "message", (e) => {
    const data = e.data;
    if (e.source !== parent || !data || data.type !== "canvas-artifact-changed") return;
    up({ type: "canvas-artifact-ack", stamp: data.stamp });
    const paths = Array.isArray(data.paths) ? data.paths.map(String) : null;
    const change = new CustomEvent("canvas-artifact-changed", { cancelable: true, detail: { paths } });
    window.dispatchEvent(change);
    if (change.defaultPrevented) return;
    if (paths && paths.length && paths.every((path) => /\.css$/i.test(path)) && swapStyles(paths)) return;
    location.reload();
  });

  try {
    for (const name of ["setTimeout", "setInterval", "requestAnimationFrame", "requestIdleCallback", "queueMicrotask"]) {
      const original = window[name];
      if (typeof original === "function") {
        window[name] = function (fn, ...rest) {
          return original.call(window, guard(fn), ...rest);
        };
      }
    }
    const proto = EventTarget.prototype;
    const unlisten = proto.removeEventListener;
    const listener = (l) => {
      if (l && typeof l === "object" && typeof l.handleEvent === "function") {
        let guarded = guards.get(l);
        if (!guarded) {
          guarded = guard(function (event) {
            return l.handleEvent(event);
          });
          guards.set(l, guarded);
        }
        return guarded;
      }
      return guard(l);
    };
    proto.addEventListener = function (type, l, options) {
      return listen.call(this, type, listener(l), options);
    };
    proto.removeEventListener = function (type, l, options) {
      return unlisten.call(this, type, (l && guards.get(l)) || l, options);
    };
    for (const target of [window, Window.prototype, Document.prototype, HTMLElement.prototype, SVGElement.prototype, Element.prototype]) {
      for (const name of Object.getOwnPropertyNames(target)) {
        if (!name.startsWith("on")) continue;
        const d = Object.getOwnPropertyDescriptor(target, name);
        if (!d || !d.set || !d.get || !d.configurable) continue;
        Object.defineProperty(target, name, {
          configurable: true,
          enumerable: d.enumerable,
          get() {
            const fn = d.get.call(this);
            return originals.get(fn) || fn;
          },
          set(fn) {
            d.set.call(this, guard(fn));
          },
        });
      }
    }
    for (const name of ["MutationObserver", "ResizeObserver", "IntersectionObserver"]) {
      const Original = window[name];
      if (typeof Original !== "function") continue;
      const Guarded = class extends Original {
        constructor(callback, ...rest) {
          super(guard(callback), ...rest);
        }
      };
      Object.defineProperty(Guarded, "name", { value: name });
      window[name] = Guarded;
    }
  } catch (_) {}
})();
