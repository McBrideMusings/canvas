// Loaded in <head> by the viewer and the Settings window so the first paint
// already has the right palette. The choice is one string under one
// localStorage key, shared by both windows; with nothing stored, the system
// setting decides. localStorage can throw, which only costs persistence.
(function () {
  var KEY = "canvas.theme";

  function read() {
    try {
      var v = localStorage.getItem(KEY);
      if (v === "light" || v === "dark") return v;
    } catch (e) {}
    return window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light";
  }

  var theme = read();

  function apply() {
    document.documentElement.setAttribute("data-theme", theme);
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
})();
