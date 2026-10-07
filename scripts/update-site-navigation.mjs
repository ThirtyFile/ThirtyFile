// Refresh the shared static navigation and section lists. GitHub Pages still publishes plain HTML, with no build.
// Run after adding a guide or changing its headings: node scripts/update-site-navigation.mjs [--language en]
import { readFileSync, writeFileSync } from "node:fs";
import { posix } from "node:path";
import { fileURLToPath } from "node:url";

const site = fileURLToPath(new URL("../site/", import.meta.url));
const source = readFileSync(`${site}assets/site.js`, "utf8");
const constant = (name) => {
  const match = source.match(new RegExp(`^\\s*const ${name} = (\\[.*\\]);$`, "m"));
  if (!match) throw new Error(`Missing ${name} list`);
  return JSON.parse(match[1]);
};
const languages = constant("LANGUAGES");
const pages = constant("TRANSLATED");
const guideMatch = source.match(/^\s*const GUIDES = (\[[\s\S]*?^\s*\]);/m);
if (!guideMatch) throw new Error("Missing GUIDES list");
const guides = JSON.parse(guideMatch[1].replace(/,(\s*[\]}])/g, "$1"));
const dictionaries = {};
for (const [, code, block] of source.matchAll(/^    "([^"]+)": \{([\s\S]*?)^    \},/gm)) {
  dictionaries[code] = Object.fromEntries([...block.matchAll(/^\s*("(?:[^"\\]|\\.)*"):\s*("(?:[^"\\]|\\.)*"),?$/gm)].map(([, key, value]) => [JSON.parse(key), JSON.parse(value)]));
}
const only = process.argv[2] === "--language" ? process.argv[3] : null;
if (only && !languages.some(([code]) => code === only)) throw new Error("Unknown language");
const base = "https://thirtyfile.github.io/ThirtyFile/";
const folder = (language) => (language === "en" ? "" : `${language}/`);
const url = (file) => base + file.replace(/(^|\/)index\.html$/, "$1");
const flat = guides.flatMap(([, items]) => items);
const guideFile = (id) => `docs/${id === "install" ? "index" : id}.html`;
const globe =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18" /></svg>';
let updated = 0;

for (const [language, name] of languages) {
  if (only && only !== language) continue;
  const t = (text) => dictionaries[language]?.[text] ?? text;
  for (const inner of pages) {
    const file = folder(language) + inner;
    let html = readFileSync(site + file, "utf8").replace(/\r\n/g, "\n");
    const relative = (target) => posix.relative(posix.dirname(file), target) || posix.basename(target);
    const local = (target) => relative(folder(language) + target);
    const isGuide = inner.startsWith("docs/");
    const id = !isGuide ? "" : inner === "docs/index.html" ? "install" : posix.basename(inner, ".html");
    const alternates = [
      ...languages.map(([code]) => `    <link rel="alternate" hreflang="${code}" href="${url(folder(code) + inner)}" />`),
      `    <link rel="alternate" hreflang="x-default" href="${url(inner)}" />`,
    ].join("\n");
    html = html.replace(/^[ \t]*<link rel="alternate"[^>]*>\r?\n/gm, "");
    html = html.replace(/(    <meta name="description"[^>]*>)/, `$1\n${alternates}`);

    const menu = `<details class="lang-menu">
            <summary>${globe}<span class="visually-hidden">${t("Language")} </span><span class="lang-name">${name}</span></summary>
            <ul>
${languages.map(([code, label]) => `              <li><a href="${relative(folder(code) + inner)}" hreflang="${code}" lang="${code}"${code === language ? ' aria-current="true"' : ""}>${label}</a></li>`).join("\n")}
            </ul>
          </details>`;
    const header = `<header class="site-header">
      <div class="wrap">
        <a class="brand" href="${local("index.html")}"><img src="${relative("assets/logo.svg")}" alt="" />ThirtyFile</a>
        <nav class="site-nav" aria-label="${t("Site")}">
          <a href="${local("index.html")}#features" class="hide-small">${t("Features")}</a>
          <a href="${local("docs/index.html")}"${isGuide ? ' aria-current="page"' : ""}>${t("Guides")}</a>
          <a href="https://github.com/ThirtyFile/ThirtyFile">GitHub</a>
          ${menu}
        </nav>
      </div>
    </header>`;
    html = html.replace(/<header class="site-header">[\s\S]*?<\/header>/, header);
    if (isGuide) {
      const links = guides
        .map(
          ([group, items]) =>
            `          <p>${t(group)}</p>\n${items.map(([guide, label]) => `          <a href="${local(guideFile(guide))}"${guide === id ? ' aria-current="page"' : ""}>${t(label)}</a>`).join("\n")}`,
        )
        .join("\n");
      html = html.replace(
        /<nav class="docs-nav"[\s\S]*?<\/nav>/,
        `<nav class="docs-nav" aria-label="${t("Guides")}">
        <details open>
          <summary>${t("All guides")}</summary>
${links}
        </details>
      </nav>`,
      );
      html = html.replace(/\s*<details class="guide-toc">[\s\S]*?<\/details>/g, "");
      const article = html.match(/<main[^>]*>([\s\S]*?)<\/main>/)?.[1] ?? "";
      const sections = [...article.matchAll(/<h2 id="([^"]+)">([\s\S]*?)<\/h2>/g)];
      if (sections.length >= 4) {
        const toc = `        <details class="guide-toc">
          <summary>${t("On this page")}</summary>
          <ol>
${sections.map(([, anchor, label]) => `            <li><a href="#${anchor}">${label.replace(/<[^>]*>/g, "")}</a></li>`).join("\n")}
          </ol>
        </details>\n`;
        html = html.replace(/^(\s*<h2\b)/m, toc + "$1");
      }
      const index = flat.findIndex(([guide]) => guide === id);
      const previous = flat[index - 1];
      const next = flat[index + 1];
      const step = (item, forward) =>
        item ? `          <a${forward ? ' class="forward"' : ""} href="${local(guideFile(item[0]))}"><span>${t(forward ? "Next" : "Previous")}</span>${t(item[1])}</a>` : "";
      const sequence = `<nav class="next" aria-label="${t("More guides")}">\n${[step(previous, false), step(next, true)].filter(Boolean).join("\n")}\n        </nav>`;
      html = html.replace(/\s*<nav class="next"[\s\S]*?<\/nav>/g, "").replace(/(\s*)<\/main>/, `\n        ${sequence}$1</main>`);
    }
    const footer = `<footer class="site-footer">
      <div class="wrap">
        <span>ThirtyFile</span>
        <a href="${local("docs/index.html")}">${t("Guides")}</a>
        <a href="https://github.com/ThirtyFile/ThirtyFile">${t("Source code")}</a>
        <a href="https://github.com/ThirtyFile/ThirtyFile/issues">${t("Report a problem")}</a>
        <a class="push" href="https://github.com/ThirtyFile/ThirtyFile/pkgs/container/thirtyfile">${t("Docker image")}</a>
        <a href="https://hub.docker.com/r/thirtyfile/thirtyfile">Docker Hub</a>
      </div>
    </footer>`;
    html = html.replace(/<footer class="site-footer">[\s\S]*?<\/footer>/, footer);
    html = html.replace(/[ \t]+$/gm, "");
    if (html !== readFileSync(site + file, "utf8")) {
      writeFileSync(site + file, html);
      updated++;
    }
  }
}
console.log(`Refreshed static navigation for ${updated} pages`);
