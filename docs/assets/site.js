(function () {
  "use strict";

  function svg(inner) {
    return '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' + inner + "</svg>";
  }
  var ICON = {
    zap: svg('<path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z"/>'),
    compass: svg('<circle cx="12" cy="12" r="9"/><path d="M15.5 8.5l-2.2 5.3-5.3 2.2 2.2-5.3 5.3-2.2z"/>'),
    book: svg('<path d="M5 4a2 2 0 012-2h11v16H7a2 2 0 00-2 2z"/><path d="M18 18v4"/>'),
    terminal: svg('<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M8 9l3 3-3 3M13 15h4"/>')
  };

  var NAV = [
    { title: "Getting Started", icon: ICON.zap, items: [
      { t: "Introduction", h: "introduction.html" },
      { t: "Installation", h: "installation.html" },
      { t: "Your first project", h: "first-project.html" },
      { t: "Commands", h: "commands.html" }
    ]},
    { title: "Guides", icon: ICON.compass, items: [
      { t: "Modules and require", h: "modules.html" },
      { t: "Roots", h: "roots.html" },
      { t: "Classes", h: "classes.html" },
      { t: "Metamethods", h: "metamethods.html" },
      { t: "Globals", h: "globals.html" },
      { t: "Including libraries", h: "libraries.html" },
      { t: "Parallel", h: "parallel.html" },
      { t: "Messenger", h: "messenger.html" },
      { t: "Native DLLs", h: "dll.html" },
      { t: "Building a program", h: "building.html" }
    ]},
    { title: "Library Reference", icon: ICON.book, items: [
      { t: "fs", h: "fs.html" },
      { t: "process", h: "process.html" },
      { t: "serde", h: "serde.html" },
      { t: "cryptography", h: "cryptography.html" },
      { t: "datetime", h: "datetime.html" },
      { t: "regex", h: "regex.html" },
      { t: "stdio", h: "stdio.html" },
      { t: "luau", h: "luau.html" },
      { t: "url", h: "url.html" },
      { t: "semver", h: "semver.html" },
      { t: "archive", h: "archive.html" },
      { t: "sqlite", h: "sqlite.html" },
      { t: "mongo", h: "mongo.html" },
      { t: "net", h: "net.html" },
      { t: "canvas", h: "canvas.html" },
      { t: "random", h: "random.html" },
      { t: "task", h: "task.html" },
      { t: "cache", h: "cache.html" }
    ]},
    { title: "Reference", icon: ICON.terminal, items: [
      { t: "CLI", h: "cli.html" },
      { t: "Editor setup", h: "editor.html" }
    ]}
  ];

  var FLAT = [];
  NAV.forEach(function (g) { g.items.forEach(function (it) { FLAT.push(it); }); });

  function currentPage() {
    var p = location.pathname.split("/").pop();
    return p && p.length ? p : "index.html";
  }

  // ---- Theme ----
  var root = document.documentElement;
  function applyThemeIcons() {
    var dark = root.getAttribute("data-theme") === "dark";
    var moon = document.getElementById("icon-moon");
    var sun = document.getElementById("icon-sun");
    if (moon) moon.style.display = dark ? "none" : "";
    if (sun) sun.style.display = dark ? "" : "none";
  }
  var themeBtn = document.getElementById("theme-btn");
  if (themeBtn) {
    applyThemeIcons();
    themeBtn.addEventListener("click", function () {
      var next = root.getAttribute("data-theme") === "dark" ? "light" : "dark";
      root.setAttribute("data-theme", next);
      try { localStorage.setItem("lehua-theme", next); } catch (e) {}
      applyThemeIcons();
    });
  }

  // ---- Mobile drawer ----
  var sidebar = document.getElementById("sidebar");
  var backdrop = document.getElementById("backdrop");
  var menuBtn = document.getElementById("menu-btn");
  function closeMenu() { if (sidebar) sidebar.classList.remove("open"); if (backdrop) backdrop.classList.remove("show"); }
  if (menuBtn) menuBtn.addEventListener("click", function () {
    sidebar.classList.toggle("open");
    if (backdrop) backdrop.classList.toggle("show");
  });
  if (backdrop) backdrop.addEventListener("click", closeMenu);

  // ---- Build sidebar ----
  var nav = document.getElementById("side-nav");
  if (nav) {
    var page = currentPage();
    var html = "";
    NAV.forEach(function (g) {
      html += '<div class="side-group">';
      html += '<div class="side-group-header"><span class="gi">' + g.icon + "</span><h4>" + g.title + "</h4></div>";
      html += '<div class="side-items">';
      g.items.forEach(function (it) {
        var active = it.h === page ? " active" : "";
        html += '<a class="side-link' + active + '" href="' + it.h + '">' + it.t + "</a>";
      });
      html += "</div></div>";
    });
    html += '<div class="side-empty" id="side-empty">No matches.</div>';
    nav.innerHTML = html;

    nav.addEventListener("click", function (e) {
      if (e.target.classList.contains("side-link")) closeMenu();
    });

    var search = document.getElementById("side-search");
    if (search) {
      search.addEventListener("input", function () {
        var q = search.value.trim().toLowerCase();
        var any = false;
        nav.querySelectorAll(".side-link").forEach(function (l) {
          var hit = l.textContent.toLowerCase().indexOf(q) !== -1;
          l.style.display = hit ? "" : "none";
          if (hit) any = true;
        });
        nav.querySelectorAll(".side-group").forEach(function (grp) {
          var vis = grp.querySelectorAll('.side-link:not([style*="none"])').length;
          grp.style.display = vis ? "" : "none";
        });
        var empty = document.getElementById("side-empty");
        if (empty) empty.style.display = any ? "none" : "block";
      });
      document.addEventListener("keydown", function (e) {
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); search.focus(); }
      });
    }
  }

  // ---- Headings: ids, anchors, and right-side TOC ----
  var content = document.querySelector(".content");
  var toc = document.getElementById("toc");
  if (content) {
    var used = {};
    function slugify(s) {
      var base = s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "section";
      var slug = base, i = 2;
      while (used[slug]) { slug = base + "-" + i; i++; }
      used[slug] = true;
      return slug;
    }
    var heads = content.querySelectorAll("h2, h3");
    var tocItems = [];
    heads.forEach(function (h) {
      if (!h.id) h.id = slugify(h.textContent);
      var a = document.createElement("a");
      a.className = "anchor"; a.href = "#" + h.id; a.textContent = "#";
      a.setAttribute("aria-hidden", "true");
      h.appendChild(a);
      tocItems.push({ id: h.id, text: h.textContent.replace(/#$/, "").trim(), level: h.tagName === "H3" ? 3 : 2 });
    });

    if (toc && tocItems.length) {
      var t = "<h5>On this page</h5>";
      tocItems.forEach(function (it) {
        t += '<a class="lvl-' + it.level + '" href="#' + it.id + '">' + it.text + "</a>";
      });
      toc.innerHTML = t;
      toc.classList.add("has-items");

      var tocLinks = {};
      toc.querySelectorAll("a").forEach(function (a) { tocLinks[a.getAttribute("href").slice(1)] = a; });
      var cur = null;
      var obs = new IntersectionObserver(function (entries) {
        entries.forEach(function (en) {
          if (en.isIntersecting && cur !== en.target.id) {
            cur = en.target.id;
            toc.querySelectorAll("a").forEach(function (a) { a.classList.remove("active"); });
            if (tocLinks[cur]) tocLinks[cur].classList.add("active");
          }
        });
      }, { rootMargin: "-90px 0px -70% 0px", threshold: 0 });
      heads.forEach(function (h) { obs.observe(h); });
    }

    // ---- Pager ----
    var article = content.querySelector(".doc-page") || content;
    var page2 = currentPage();
    var idx = -1;
    for (var i = 0; i < FLAT.length; i++) { if (FLAT[i].h === page2) { idx = i; break; } }
    if (idx !== -1) {
      var prev = idx > 0 ? FLAT[idx - 1] : null;
      var next = idx < FLAT.length - 1 ? FLAT[idx + 1] : null;
      var pager = document.createElement("nav");
      pager.className = "pager";
      var out = "";
      if (prev) out += '<a class="prev" href="' + prev.h + '"><span class="dir">Previous</span><span class="pt">' + prev.t + "</span></a>";
      if (next) out += '<a class="next" href="' + next.h + '"><span class="dir">Next</span><span class="pt">' + next.t + "</span></a>";
      pager.innerHTML = out;
      article.appendChild(pager);
    }
  }

  // ---- Syntax highlighting ----
  var LUAU_KW = "local function end if then else elseif for in do while repeat until return break continue and or not nil true false type export".split(" ");
  var LUAU_GLOBAL = "require parallel channel messenger __dirname __filename fs process serde cryptography datetime regex stdio luau url semver archive sqlite mongo net random task canvas cache dll buffer print assert pcall error tostring tonumber pairs ipairs select typeof GetType NewClassData BuildClassData SuperGet Interface Implements".split(" ");
  var kw = {}, gl = {};
  LUAU_KW.forEach(function (w) { kw[w] = 1; });
  LUAU_GLOBAL.forEach(function (w) { gl[w] = 1; });

  function esc(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
  function span(cls, text) { return '<span class="tok-' + cls + '">' + esc(text) + "</span>"; }
  function isDigit(c) { return c >= "0" && c <= "9"; }
  function isIdentStart(c) { return (c >= "a" && c <= "z") || (c >= "A" && c <= "Z") || c === "_"; }
  function isIdent(c) { return isIdentStart(c) || isDigit(c); }

  function highlightLuau(code) {
    var out = "", i = 0, n = code.length;
    while (i < n) {
      var c = code[i];
      if (c === "-" && code[i + 1] === "-") {
        var j = i + 2, end;
        if (code[j] === "[") {
          var k = j + 1, eq = 0;
          while (code[k] === "=") { eq++; k++; }
          if (code[k] === "[") {
            var close = "]" + Array(eq + 1).join("=") + "]";
            end = code.indexOf(close, k + 1); end = end === -1 ? n : end + close.length;
            out += span("comment", code.slice(i, end)); i = end; continue;
          }
        }
        end = code.indexOf("\n", i); if (end === -1) end = n;
        out += span("comment", code.slice(i, end)); i = end; continue;
      }
      if (c === '"' || c === "'" || c === "`") {
        var j2 = i + 1;
        while (j2 < n) {
          if (code[j2] === "\\") { j2 += 2; continue; }
          if (code[j2] === c) { j2++; break; }
          if (code[j2] === "\n" && c !== "`") break;
          j2++;
        }
        out += span("string", code.slice(i, j2)); i = j2; continue;
      }
      if (c === "[") {
        var k2 = i + 1, eq2 = 0;
        while (code[k2] === "=") { eq2++; k2++; }
        if (code[k2] === "[") {
          var close2 = "]" + Array(eq2 + 1).join("=") + "]";
          var end2 = code.indexOf(close2, k2 + 1); end2 = end2 === -1 ? n : end2 + close2.length;
          out += span("string", code.slice(i, end2)); i = end2; continue;
        }
      }
      if (isDigit(c) || (c === "." && isDigit(code[i + 1] || ""))) {
        var j3 = i + 1;
        while (j3 < n && /[0-9a-fA-FxXeE._]/.test(code[j3])) j3++;
        out += span("number", code.slice(i, j3)); i = j3; continue;
      }
      if (isIdentStart(c)) {
        var j4 = i + 1;
        while (j4 < n && isIdent(code[j4])) j4++;
        var word = code.slice(i, j4);
        if (kw[word]) out += span("keyword", word);
        else if (gl[word]) out += span("global", word);
        else {
          var k4 = j4;
          while (k4 < n && (code[k4] === " " || code[k4] === "\t")) k4++;
          if (code[k4] === "(") out += span("func", word);
          else out += esc(word);
        }
        i = j4; continue;
      }
      out += esc(c); i++;
    }
    return out;
  }

  function highlightGeneric(code) {
    var out = "", i = 0, n = code.length, atLineStart = true;
    while (i < n) {
      var c = code[i];
      if (c === "\n") { out += "\n"; i++; atLineStart = true; continue; }
      if (c === " " || c === "\t") { out += c; i++; continue; }
      if (c === "#" || (c === "/" && code[i + 1] === "/")) {
        var end = code.indexOf("\n", i); if (end === -1) end = n;
        out += span("comment", code.slice(i, end)); i = end; atLineStart = false; continue;
      }
      if (atLineStart && c === "[") {
        var end2 = code.indexOf("]", i); if (end2 === -1) end2 = n - 1;
        out += span("section", code.slice(i, end2 + 1)); i = end2 + 1; atLineStart = false; continue;
      }
      if (c === '"' || c === "'") {
        var j = i + 1;
        while (j < n && code[j] !== c && code[j] !== "\n") { if (code[j] === "\\") j++; j++; }
        j = Math.min(j + 1, n);
        out += span("string", code.slice(i, j)); i = j; atLineStart = false; continue;
      }
      if (isDigit(c)) {
        var j2 = i + 1;
        while (j2 < n && /[0-9._]/.test(code[j2])) j2++;
        out += span("number", code.slice(i, j2)); i = j2; atLineStart = false; continue;
      }
      out += esc(c); i++; atLineStart = false;
    }
    return out;
  }

  document.querySelectorAll("pre code").forEach(function (block) {
    var text = block.textContent;
    var cls = block.className || "";
    block.innerHTML = cls.indexOf("language-luau") !== -1 ? highlightLuau(text) : highlightGeneric(text);

    var pre = block.parentElement;
    var wrap = pre.parentElement;
    if (wrap && wrap.classList.contains("codeblock")) {
      if (wrap.querySelector(".window-bar")) wrap.classList.add("has-bar");
      var btn = document.createElement("button");
      btn.className = "copy-btn"; btn.type = "button"; btn.textContent = "Copy";
      btn.addEventListener("click", function () {
        navigator.clipboard.writeText(text).then(function () {
          btn.textContent = "Copied";
          setTimeout(function () { btn.textContent = "Copy"; }, 1400);
        });
      });
      wrap.appendChild(btn);
    }
  });
})();
