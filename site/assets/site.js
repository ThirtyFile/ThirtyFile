// ThirtyFile website: the shared header, guide menu and footer, plus the language and theme switches, copy buttons
// and tabs.
// Pages say where the site root is with <html data-root="…"> ("." for the home page, ".." for the guides, one level
// more in a language's folder) and their language with <html lang>.
// Each page also has the header (with the language switch), the guide menu and the footer in its HTML, for browsers
// without JavaScript; the guide menu is made again here from GUIDES, so a new guide only needs adding below to appear
// in every menu.

(() => {
  const html = document.documentElement;
  const root = html.dataset.root || ".";
  const page = html.dataset.page || "";
  const REPO = "https://github.com/ThirtyFile/ThirtyFile";

  // The website's languages, English first, and the pages that exist in every one of them, by their path inside a
  // language's folder (English is at the root, the others in site/<code>/). Every other page is in English only.
  // scripts/check-site.mjs reads both lists (keep each on one line): it fails when a page below is missing in a
  // language, or when the language links of the pages don't match.
  const LANGUAGES = [["en", "English"], ["zh-TW", "繁體中文"]];
  const TRANSLATED = ["index.html", "docs/index.html", "docs/first-steps.html"];

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

  // The texts this script writes, by their English text. The pages' own texts are in their HTML. The words follow
  // the glossary in web/src/lib/i18n/glossary.md
  const DICT = {
    "zh-TW": {
      "Site": "網站",
      "Features": "功能",
      "Guides": "指南",
      "Language": "語言",
      "Skip to content": "跳到主要內容",
      "Switch between light and dark": "切換淺色與深色",
      "This page is also available in English.": "這個頁面也有繁體中文版。",
      "Read it in English": "改看繁體中文版",
      "Close": "關閉",
      "All guides": "所有指南",
      "Get started": "開始使用",
      "Install": "安裝",
      "First steps": "初次設定",
      "Everyday use": "日常使用",
      "Working with files": "檔案操作",
      "Preview and editing": "預覽與編輯",
      "Sharing": "分享",
      "Your account": "你的帳號",
      "Mapping it as a drive": "連線為網路磁碟機",
      "Administration": "管理",
      "Users and spaces": "使用者與空間",
      "Single sign-on": "單一登入",
      "Storage locations": "儲存位置",
      "Logs and branding": "紀錄與品牌",
      "Running it": "架設與維運",
      "Settings": "設定",
      "Putting it on the internet": "從網際網路存取",
      "Upgrade and backup": "升級與備份",
      "Troubleshooting": "疑難排解",
      "The project": "專案",
      "Contributing": "參與貢獻",
      "Previous": "上一篇",
      "Next": "下一篇",
      "More guides": "更多指南",
      "Source code": "原始碼",
      "Report a problem": "回報問題",
      "Docker image": "Docker 映像檔",
      "Copy": "複製",
      "Copied": "已複製",
      "Select and copy": "請選取後複製",
    },
  };

  const lang = LANGUAGES.some(([code]) => code === html.lang) ? html.lang : "en";
  const t = (text, code = lang) => DICT[code]?.[text] ?? text;
  const el = (markup) => {
    const tpl = document.createElement("template");
    tpl.innerHTML = markup.trim();
    return tpl.content.firstElementChild;
  };
  // This page's path inside its language's folder, and the address of a page in a language
  const path = page ? `docs/${page === "install" ? "index" : page}.html` : "index.html";
  const at = (code, file) => `${root}/${code === "en" ? "" : `${code}/`}${file}`;
  // A page in this page's language when it is translated, in English otherwise
  const link = (file) => at(TRANSLATED.includes(file) ? lang : "en", file);
  const guideUrl = (id) => link(`docs/${id === "install" ? "index" : id}.html`);
  // The same page in another language, or that language's home page when this page isn't translated
  const counterpart = (code) => at(code, TRANSLATED.includes(path) ? path : "index.html");

  const storage = {
    get(key) {
      try {
        return localStorage.getItem(key);
      } catch {
        return null;
      }
    },
    set(key, value) {
      try {
        localStorage.setItem(key, value);
      } catch {
        // private mode: the choice lasts for this page only
      }
    },
  };

  // ── Theme: follows the system until the visitor picks one ──
  const stored = storage.get("tf-site-theme");
  if (stored === "light" || stored === "dark") html.dataset.theme = stored;
  const isDark = () => (html.dataset.theme ?? (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light")) === "dark";

  // ── Header, and the link past it to the content ──
  const GLOBE =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/></svg>';
  const languageMenu = () => `
    <details class="lang-menu">
      <summary>${GLOBE}<span class="visually-hidden">${t("Language")} </span><span class="lang-name">${LANGUAGES.find(([code]) => code === lang)[1]}</span></summary>
      <ul>${LANGUAGES.map(([code, name]) => `<li><a href="${counterpart(code)}" hreflang="${code}" lang="${code}"${code === lang ? ' aria-current="true"' : ""}>${name}</a></li>`).join("")}</ul>
    </details>`;
  let header = document.querySelector(".site-header");
  if (!header) {
    header = el(`
      <header class="site-header">
        <div class="wrap">
          <a class="brand" href="${link("index.html")}"><img src="${root}/assets/logo.svg" alt="" />ThirtyFile</a>
          <nav class="site-nav" aria-label="${t("Site")}">
            <a href="${link("index.html")}#features" class="hide-small">${t("Features")}</a>
            <a href="${guideUrl("install")}" ${page ? 'aria-current="page"' : ""}>${t("Guides")}</a>
            <a href="${REPO}">GitHub</a>
          </nav>
        </div>
      </header>`);
    document.body.prepend(header);
  }
  const siteNav = header.querySelector(".site-nav");
  if (LANGUAGES.length > 1 && !siteNav.querySelector(".lang-menu")) siteNav.append(el(languageMenu()));
  const main = document.querySelector("main");
  if (main && !document.querySelector(".skip-link")) {
    main.id ||= "content";
    document.body.prepend(el(`<a class="skip-link" href="#${main.id}">${t("Skip to content")}</a>`));
  }
  // The theme switch needs JavaScript, so only this adds it
  const toggle = el(`
    <button class="theme-toggle" type="button" aria-label="${t("Switch between light and dark")}">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/></svg>
    </button>`);
  siteNav.append(toggle);
  toggle.addEventListener("click", () => {
    const next = isDark() ? "light" : "dark";
    html.dataset.theme = next;
    storage.set("tf-site-theme", next);
  });

  // ── Language switch: plain links, so it works without JavaScript; this closes it like a menu ──
  // A visitor who picks a language, or was offered one, isn't offered it again
  const OFFERED = "tf-site-language-offered";
  const menu = siteNav.querySelector(".lang-menu");
  if (menu) {
    for (const a of menu.querySelectorAll("a")) a.addEventListener("click", () => storage.set(OFFERED, "1"));
    document.addEventListener("click", (e) => {
      if (menu.open && !menu.contains(e.target)) menu.open = false;
    });
    menu.addEventListener("keydown", (e) => {
      if (e.key !== "Escape" || !menu.open) return;
      menu.open = false;
      menu.querySelector("summary").focus();
    });
  }

  // ── Offer the browser's language once, on a page that has it, without leaving the page ──
  const browserLanguage = () => {
    for (const tag of navigator.languages?.length ? navigator.languages : [navigator.language ?? ""]) {
      const l = tag.toLowerCase();
      const code = l.startsWith("ja")
        ? "ja"
        : /^zh-(hant|tw|hk|mo)\b/.test(l)
          ? "zh-TW"
          : l === "zh" || l.startsWith("zh-")
            ? "zh-CN"
            : l.startsWith("en")
              ? "en"
              : null;
      if (code && LANGUAGES.some(([c]) => c === code)) return code;
    }
    return null;
  };
  const wanted = browserLanguage();
  if (wanted && wanted !== lang && TRANSLATED.includes(path) && !storage.get(OFFERED)) {
    storage.set(OFFERED, "1");
    // Written in the language offered, which the visitor reads
    const offer = el(`
      <div class="lang-offer" lang="${wanted}" role="region" aria-label="${t("Language", wanted)}">
        <div class="wrap">
          <p>${t("This page is also available in English.", wanted)}</p>
          <a href="${counterpart(wanted)}" hreflang="${wanted}">${t("Read it in English", wanted)}</a>
          <button type="button" aria-label="${t("Close", wanted)}">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>
          </button>
        </div>
      </div>`);
    header.after(offer);
    offer.querySelector("button").addEventListener("click", () => offer.remove());
  }

  // ── Guide menu and previous / next links ──
  const nav = document.querySelector(".docs-nav");
  if (nav) {
    // Links to guides in another language say so, for the browser and screen readers
    const guideLink = (id, attrs = "") => {
      const href = guideUrl(id);
      const other = lang !== "en" && !TRANSLATED.includes(`docs/${id === "install" ? "index" : id}.html`);
      return `href="${href}"${other ? ' hreflang="en"' : ""}${attrs}`;
    };
    const links = GUIDES.map(
      ([group, items]) => `<p>${t(group)}</p>` + items.map(([id, title]) => `<a ${guideLink(id, id === page ? ' aria-current="page"' : "")}>${t(title)}</a>`).join(""),
    ).join("");
    nav.innerHTML = `<details><summary>${t("All guides")}</summary>${links}</details>`;
    const details = nav.querySelector("details");
    const wide = matchMedia("(min-width: 900px)");
    const sync = () => (details.open = wide.matches);
    sync();
    wide.addEventListener("change", sync);

    const flat = GUIDES.flatMap(([, items]) => items);
    const index = flat.findIndex(([id]) => id === page);
    const article = document.querySelector(".article");
    if (article && index >= 0) {
      const prev = flat[index - 1];
      const next = flat[index + 1];
      article.append(
        el(`<nav class="next" aria-label="${t("More guides")}">
          ${prev ? `<a ${guideLink(prev[0])}><span>${t("Previous")}</span>${t(prev[1])}</a>` : ""}
          ${next ? `<a class="forward" ${guideLink(next[0])}><span>${t("Next")}</span>${t(next[1])}</a>` : ""}
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
          <a href="${guideUrl("install")}">${t("Guides")}</a>
          <a href="${REPO}">${t("Source code")}</a>
          <a href="${REPO}/issues">${t("Report a problem")}</a>
          <a class="push" href="${REPO}/pkgs/container/thirtyfile">${t("Docker image")}</a>
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
    const button = el(`<button class="copy" type="button">${t("Copy")}</button>`);
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(block.innerText.trim());
        button.textContent = t("Copied");
      } catch {
        button.textContent = t("Select and copy");
      }
      setTimeout(() => (button.textContent = t("Copy")), 1800);
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
