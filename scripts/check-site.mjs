// Checks the website in site/ (scripts/check.sh site):
//  1. every page in TRANSLATED (site/assets/site.js) exists in every language of LANGUAGES: English at the root, the
//     others in their folder (site/zh-TW/…), and nothing else is in a language's folder
//  2. each version says its language in <html lang>, has a title and a description of its own, and lists every
//     version, itself included, in <link rel="alternate" hreflang>, plus x-default for English; pages that aren't
//     translated have no such links
//  3. the language switch in each page's header links to every language once, in the order of LANGUAGES: to the same
//     page when it is translated, to that language's home page otherwise
//  4. the links and pictures inside the site lead to files that exist, and their #anchors to ids on those pages
// Usage: node scripts/check-site.mjs
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { posix } from "node:path";
import { fileURLToPath } from "node:url";

const site = fileURLToPath(new URL("../site/", import.meta.url));
// Where GitHub Pages publishes site/
const BASE = "https://thirtyfile.github.io/ThirtyFile/";

const script = readFileSync(`${site}assets/site.js`, "utf8");
const list = (name) => {
  const found = script.match(new RegExp(`^\\s*const ${name} = (\\[.*\\]);$`, "m"));
  if (!found) {
    console.error(`site/assets/site.js: no \`const ${name} = [...];\` on one line`);
    process.exit(2);
  }
  return JSON.parse(found[1]);
};
const LANGUAGES = list("LANGUAGES").map(([code]) => code);
const TRANSLATED = list("TRANSLATED");

const errors = [];
const fail = (file, message) => errors.push(`site/${file}: ${message}`);

// Every file under site/, by its path with forward slashes
const walk = (dir) =>
  readdirSync(site + dir).flatMap((name) => {
    const file = dir ? `${dir}/${name}` : name;
    return statSync(site + file).isDirectory() ? walk(file) : [file];
  });
const files = new Set(walk(""));
const pages = [...files].filter((file) => file.endsWith(".html"));
const text = new Map(pages.map((file) => [file, readFileSync(site + file, "utf8")]));

// The language of a page and its path inside the language's folder
const where = (file) => {
  const [first, ...rest] = file.split("/");
  return LANGUAGES.includes(first) && first !== "en" ? [first, rest.join("/")] : ["en", file];
};
const folder = (code) => (code === "en" ? "" : `${code}/`);
// The published address of a page: folders end in / instead of index.html
const published = (file) => BASE + file.replace(/(^|\/)index\.html$/, "$1");
const attr = (tag, name) => tag.match(new RegExp(`\\s${name}="([^"]*)"`))?.[1];
const tags = (html, name) => [...html.matchAll(new RegExp(`<${name}\\b[^>]*>`, "g"))].map((m) => m[0]);
const ids = (html) => new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1]));
const titleOf = (html) => html.match(/<title>([^<]*)<\/title>/)?.[1].trim() ?? "";
const descriptionOf = (html) => attr(tags(html, "meta").find((tag) => attr(tag, "name") === "description") ?? "", "content")?.trim() ?? "";

// ── 1. Every translated page in every language, and nothing else in their folders ──
for (const dir of readdirSync(site)) {
  if (statSync(site + dir).isDirectory() && !["assets", "docs", ...LANGUAGES].includes(dir) && /^[a-z]{2}(-[A-Z]{2})?$/.test(dir)) {
    fail(dir, `looks like a language folder, but "${dir}" isn't in LANGUAGES in site/assets/site.js`);
  }
}
for (const code of LANGUAGES) {
  for (const file of TRANSLATED) {
    if (!files.has(folder(code) + file)) fail(folder(code) + file, `missing: every page in TRANSLATED needs a version in each language (${LANGUAGES.join(", ")})`);
  }
}
for (const file of pages) {
  const [code, inner] = where(file);
  if (code !== "en" && !TRANSLATED.includes(inner)) fail(file, `isn't in TRANSLATED in site/assets/site.js; only the pages listed there are translated`);
}

for (const file of pages) {
  const html = text.get(file);
  const [code, inner] = where(file);
  const translated = TRANSLATED.includes(inner);

  // ── 2. Language, title, description and hreflang links ──
  const lang = attr(tags(html, "html")[0] ?? "", "lang");
  if (lang !== code) fail(file, `<html lang="${lang ?? ""}"> should be "${code}"`);
  const title = titleOf(html);
  const description = descriptionOf(html);
  if (!title) fail(file, "has no <title>");
  if (!description) fail(file, 'has no <meta name="description">');
  if (code !== "en" && translated && text.has(inner)) {
    if (title && title === titleOf(text.get(inner))) fail(file, `its <title> is the English one`);
    if (description && description === descriptionOf(text.get(inner))) fail(file, "its description is the English one");
  }

  const expected = translated ? [...LANGUAGES.map((c) => [c, published(folder(c) + inner)]), ["x-default", published(inner)]] : [];
  const found = tags(html, "link")
    .filter((tag) => attr(tag, "rel") === "alternate" && attr(tag, "hreflang"))
    .map((tag) => [attr(tag, "hreflang"), attr(tag, "href")]);
  const show = (pairs) => pairs.map(([c, href]) => `${c} ${href}`).join(", ") || "none";
  if (show(found) !== show(expected)) {
    fail(file, `hreflang links should be, in this order: ${show(expected)}\n    found: ${show(found)}`);
  }

  // ── 3. The language switch in the header ──
  const header = html.match(/<header class="site-header">([\s\S]*?)<\/header>/)?.[1];
  const menu = header?.match(/<details class="lang-menu">([\s\S]*?)<\/details>/)?.[1];
  if (LANGUAGES.length > 1 && !menu) {
    fail(file, 'has no language switch (<details class="lang-menu">) in its header');
  } else if (menu) {
    const want = LANGUAGES.map((c) => `${c} ${folder(c) + (translated ? inner : "index.html")}${c === code ? " (current)" : ""}`);
    const have = tags(menu, "a").map((tag) => {
      const target = posix.normalize(posix.join(posix.dirname(file), attr(tag, "href") ?? ""));
      const current = attr(tag, "aria-current") === "true" ? " (current)" : "";
      const named = attr(tag, "lang") === attr(tag, "hreflang") ? "" : " (lang differs from hreflang)";
      return `${attr(tag, "hreflang")} ${target}${current}${named}`;
    });
    if (want.join(", ") !== have.join(", ")) fail(file, `the language switch should link to: ${want.join(", ")}\n    found: ${have.join(", ")}`);
  }
}

// ── 4. Links and anchors inside the site ──
for (const file of pages) {
  const html = text.get(file);
  for (const [, url] of html.matchAll(/\s(?:href|src)="([^"]*)"/g)) {
    let target;
    if (url.startsWith(BASE)) target = url.slice(BASE.length);
    else if (/^([a-z]+:|\/\/)/i.test(url)) continue;
    else target = url.startsWith("#") ? file + url : posix.normalize(posix.join(posix.dirname(file), url));
    const [path, anchor] = target.split("#");
    const resolved = path === "" || path.endsWith("/") ? `${path}index.html` : path;
    if (!files.has(resolved)) {
      fail(file, `links to ${url}, which doesn't exist`);
    } else if (anchor && resolved.endsWith(".html") && !ids(text.get(resolved)).has(anchor)) {
      fail(file, `links to ${url}, but site/${resolved} has no id="${anchor}"`);
    }
  }
}

if (errors.length) {
  console.error(errors.join("\n"));
  console.error(`\n${errors.length} problem(s) in the website`);
  process.exit(1);
}
console.log(`Website: ${pages.length} pages; ${TRANSLATED.length} in each of ${LANGUAGES.join(", ")}; links and anchors resolve`);
