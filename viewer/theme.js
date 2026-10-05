// Loaded in <head> by the viewer and the Settings window so the first paint
// already has the right palette. The choice is one string under one
// localStorage key, shared by both windows; with nothing stored, the system
// setting decides. localStorage can throw, which only costs persistence.
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

  window.canvasTheme = {
    get: function () {
      return theme;
    },
    set: function (next) {
      if (next !== "light" && next !== "dark") return;
      theme = next;
      try {
        localStorage.setItem(KEY, theme);
      } catch (e) {}
      apply();
    },
  };

  window.addEventListener("storage", function (e) {
    if (e.key !== KEY) return;
    var next = read();
    if (next === theme) return;
    theme = next;
    apply();
  });

  document.documentElement.setAttribute("data-theme", theme);
  syncApp();
})();
