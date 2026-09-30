// ThirtyFile website: the shared header, guide menu and footer, plus the theme switch, copy buttons and tabs.
// Pages say where the site root is with <html data-root="…"> ("." for the home page, ".." for the guides).
// Each page also has the header, the guide menu and the footer in its HTML, for browsers without JavaScript; the
// guide menu is made again here from GUIDES, so a new guide only needs adding below to appear in every menu.

(() => {
  const root = document.documentElement.dataset.root || ".";
  const page = document.documentElement.dataset.page || "";
  const REPO = "https://github.com/ThirtyFile/ThirtyFile";

  const GUIDES = [
    ["Get started", [
      ["install", "Install"],
      ["first-steps", "First steps"],
    ]],
    ["Everyday use", [
      ["files", "Working with files"],
      ["preview", "Preview and editing"],
      ["sharing", "Sharing"],
      ["account", "Your account"],
      ["webdav", "Mapping it as a drive"],
    ]],
    ["Administration", [
      ["users", "Users and spaces"],
      ["sign-in", "Single sign-on"],
      ["storage", "Storage locations"],
      ["logs", "Logs and branding"],
    ]],
    ["Running it", [
      ["settings", "Settings"],
      ["internet", "Putting it on the internet"],
      ["backup", "Upgrade and backup"],
      ["help", "Troubleshooting"],
    ]],
    ["The project", [
      ["contributing", "Contributing"],
    ]],
  ];

  const el = (html) => {
    const t = document.createElement("template");
    t.innerHTML = html.trim();
    return t.content.firstElementChild;
  };
  const guideUrl = (id) => `${root}/docs/${id === "install" ? "index" : id}.html`;

  // ── Theme: follows the system until the visitor picks one ──
  const stored = (() => {
    try {
      return localStorage.getItem("tf-site-theme");
    } catch {
      return null;
    }
  })();
  if (stored === "light" || stored === "dark") document.documentElement.dataset.theme = stored;
  const isDark = () => (document.documentElement.dataset.theme ?? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")) === "dark";

  // ── Header, and the link past it to the content ──
  let header = document.querySelector(".site-header");
  if (!header) {
    header = el(`
      <header class="site-header">
        <div class="wrap">
          <a class="brand" href="${root}/index.html"><img src="${root}/assets/logo.svg" alt="" />ThirtyFile</a>
          <nav class="site-nav" aria-label="Site">
            <a href="${root}/index.html#features" class="hide-small">Features</a>
            <a href="${guideUrl("install")}" ${page ? 'aria-current="page"' : ""}>Guides</a>
            <a href="${REPO}">GitHub</a>
          </nav>
        </div>
      </header>`);
    document.body.prepend(header);
  }
  const main = document.querySelector("main");
  if (main && !document.querySelector(".skip-link")) {
    main.id ||= "content";
    document.body.prepend(el(`<a class="skip-link" href="#${main.id}">Skip to content</a>`));
  }
  // The theme switch needs JavaScript, so only this adds it
  const toggle = el(`
    <button class="theme-toggle" type="button" aria-label="Switch between light and dark">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg>
    </button>`);
  header.querySelector(".site-nav").append(toggle);
  toggle.addEventListener("click", () => {
    const next = isDark() ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem("tf-site-theme", next);
    } catch {
      // private mode: the choice lasts for this page only
    }
  });

  // ── Guide menu and previous / next links ──
  const nav = document.querySelector(".docs-nav");
  if (nav) {
    const links = GUIDES.map(
      ([group, items]) => `<p>${group}</p>` + items.map(([id, title]) => `<a href="${guideUrl(id)}" ${id === page ? 'aria-current="page"' : ""}>${title}</a>`).join(""),
    ).join("");
    nav.innerHTML = `<details><summary>All guides</summary>${links}</details>`;
    const details = nav.querySelector("details");
    const wide = matchMedia("(min-width: 900px)");
    const sync = () => (details.open = wide.matches);
    sync();
    wide.addEventListener("change", sync);

    const flat = GUIDES.flatMap(([, items]) => items);
    const at = flat.findIndex(([id]) => id === page);
    const article = document.querySelector(".article");
    if (article && at >= 0) {
      const prev = flat[at - 1];
      const next = flat[at + 1];
      article.append(
        el(`<nav class="next" aria-label="More guides">
          ${prev ? `<a href="${guideUrl(prev[0])}"><span>Previous</span>${prev[1]}</a>` : ""}
          ${next ? `<a class="forward" href="${guideUrl(next[0])}"><span>Next</span>${next[1]}</a>` : ""}
        </nav>`),
      );
    }
  }

  // ── Footer ──
  if (!document.querySelector(".site-footer")) {
    document.body.append(
      el(`
      <footer class="site-footer">
        <div class="wrap">
          <span>ThirtyFile</span>
          <a href="${guideUrl("install")}">Guides</a>
          <a href="${REPO}">Source code</a>
          <a href="${REPO}/issues">Report a problem</a>
          <a class="push" href="${REPO}/pkgs/container/thirtyfile">Docker image</a>
        </div>
      </footer>`),
    );
  }

  // ── Copy buttons on code blocks ──
  for (const block of document.querySelectorAll("pre")) {
    const box = document.createElement("div");
    box.className = "code";
    block.replaceWith(box);
    box.append(block);
    const button = el('<button class="copy" type="button">Copy</button>');
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(block.innerText.trim());
        button.textContent = "Copied";
      } catch {
        button.textContent = "Select and copy";
      }
      setTimeout(() => (button.textContent = "Copy"), 1800);
    });
    box.append(button);
  }

  // ── Tabs (install options on the home page) ──
  for (const tabs of document.querySelectorAll(".tabs")) {
    const buttons = [...tabs.querySelectorAll("button")];
    // Each tab names the panel it shows, and the panel is named by its tab
    for (const b of buttons) {
      b.id ||= `${b.dataset.tab}-tab`;
      b.setAttribute("aria-controls", b.dataset.tab);
      document.getElementById(b.dataset.tab).setAttribute("aria-labelledby", b.id);
    }
    const show = (id) => {
      for (const b of buttons) {
        const on = b.dataset.tab === id;
        b.setAttribute("aria-selected", String(on));
        b.tabIndex = on ? 0 : -1;
        document.getElementById(b.dataset.tab).hidden = !on;
      }
    };
    buttons.forEach((b, i) => {
      b.addEventListener("click", () => show(b.dataset.tab));
      b.addEventListener("keydown", (e) => {
        if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
        const next = buttons[(i + (e.key === "ArrowRight" ? 1 : buttons.length - 1)) % buttons.length];
        show(next.dataset.tab);
        next.focus();
      });
    });
    show(buttons[0].dataset.tab);
  }
})();
