/* ~1 KB of progressive enhancement: theme persistence, mobile nav, and
   confirmation for destructive forms. Everything else is server-rendered. */
(function () {
  "use strict";

  var root = document.documentElement;

  // Theme -------------------------------------------------------------------
  var stored = null;
  try {
    stored = localStorage.getItem("wp-panel-theme");
  } catch (e) {}
  if (stored) root.setAttribute("data-theme", stored);

  document.addEventListener("click", function (event) {
    var toggle = event.target.closest("[data-theme-toggle]");
    if (toggle) {
      var next = root.getAttribute("data-theme") === "light" ? "dark" : "light";
      root.setAttribute("data-theme", next);
      try {
        localStorage.setItem("wp-panel-theme", next);
      } catch (e) {}
      return;
    }

    // Mobile navigation ----------------------------------------------------
    if (event.target.closest("[data-nav-toggle]")) {
      document.body.classList.toggle("nav-open");
      return;
    }
    if (document.body.classList.contains("nav-open") && !event.target.closest(".sidebar")) {
      document.body.classList.remove("nav-open");
    }
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") document.body.classList.remove("nav-open");
    // "/" focuses the first search box on the page.
    if (event.key === "/" && !/^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)) {
      var search = document.querySelector("input[type=search]");
      if (search) {
        event.preventDefault();
        search.focus();
      }
    }
  });

  // Destructive actions ----------------------------------------------------
  document.addEventListener("submit", function (event) {
    var form = event.target;
    var message = form.getAttribute("data-confirm");
    if (message && !window.confirm(message)) event.preventDefault();
  });

  // Flash messages fade out after a while without shifting layout.
  var flash = document.querySelector("[data-flash]");
  if (flash) {
    setTimeout(function () {
      flash.style.transition = "opacity .4s ease";
      flash.style.opacity = "0";
      setTimeout(function () {
        flash.remove();
      }, 400);
    }, 6000);
  }
})();
