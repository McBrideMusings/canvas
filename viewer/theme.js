// Loaded in <head> by the viewer and the Settings window so the first paint
// already has the right palette. The choice is one string under one
// localStorage key, shared by both windows; with nothing stored, the system
// setting decides, and set("system") removes the key to get back there.
// localStorage can throw, which only costs persistence.
// Canvas.app's own appearance follows the choice too (set_app_theme), so
// `prefers-color-scheme` in every frame, an artifact's pane included, agrees
// with the theme the window shows.
(function () {
  var KEY = "canvas.theme";

  function stored() {
    try {
      var v = localStorage.getItem(KEY);
      if (v === "light" || v === "dark") return v;
    } catch (e) {}
    return null;
  }

  function read() {
    return (
      stored() ||
      (window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches
        ? "dark"
        : "light")
    );
  }

  // Only Canvas.app's WKWebView injects window.__TAURI__; null hands the
  // appearance back to the system.
  function syncApp() {
    var core = window.__TAURI__ && window.__TAURI__.core;
    if (core) core.invoke("set_app_theme", { theme: stored() }).catch(function () {});
  }

  var theme = read();

  function apply() {
    document.documentElement.setAttribute("data-theme", theme);
    syncApp();
    window.dispatchEvent(new CustomEvent("canvas-theme", { detail: theme }));
  }

  // After the stored choice or the system's answer moved: a new theme repaints
  // through apply(); the same theme only changed its source, which the app's
  // appearance and canvas-theme-source listeners still need to hear, without
  // rebuilding anything that draws the theme.
  function show(next) {
    if (next !== theme) {
      theme = next;
      apply();
      return;
    }
    syncApp();
    window.dispatchEvent(new CustomEvent("canvas-theme-source", { detail: theme }));
  }

  window.canvasTheme = {
    get: function () {
      return theme;
    },
    // "stored" while a choice is in localStorage, else "system".
    source: function () {
      return stored() === null ? "system" : "stored";
    },
    set: function (next) {
      if (next === "system") {
        try {
          localStorage.removeItem(KEY);
        } catch (e) {}
        show(read());
        return;
      }
      if (next !== "light" && next !== "dark") return;
      try {
        localStorage.setItem(KEY, next);
      } catch (e) {}
      show(next);
    },
  };

  window.addEventListener("storage", function (e) {
    if (e.key === KEY) show(read());
  });

  // With nothing stored, the window follows the system. While a choice is
  // stored, the app's forced appearance answers the media query, so the
  // system's own answer arrives here once set_app_theme(null) releases it.
  if (window.matchMedia) {
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", function () {
      if (stored() === null) show(read());
    });
  }

  document.documentElement.setAttribute("data-theme", theme);
  syncApp();
})();
