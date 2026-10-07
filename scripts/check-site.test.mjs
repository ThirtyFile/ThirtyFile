import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const checker = fileURLToPath(new URL("./check-site.mjs", import.meta.url));
const languages = ["en", "zh-TW", "zh-CN", "ja"];
const pages = ["index.html", "docs/webdav.html"];
const base = "https://thirtyfile.github.io/ThirtyFile/";
const prefix = (language) => (language === "en" ? "" : `${language}/`);
const published = (file) => base + file.replace(/(^|\/)index\.html$/, "$1");

function fixture(change, expected) {
  const temporary = mkdtempSync(join(tmpdir(), "thirtyfile-site-check-"));
  const safeRoot = resolve(tmpdir()) + sep;
  if (!resolve(temporary).startsWith(safeRoot)) throw new Error("Unsafe fixture cleanup path");
  try {
    const put = (file, text) => {
      mkdirSync(dirname(join(temporary, file)), { recursive: true });
      writeFileSync(join(temporary, file), text);
    };
    put("assets/site.js", `const LANGUAGES = ${JSON.stringify(languages.map((code) => [code, code]))};\nconst TRANSLATED = ${JSON.stringify(pages)};`);
    for (const language of languages) {
      for (const page of pages) {
        const file = prefix(language) + page;
        const alternate = [
          ...languages.map((code) => `<link rel="alternate" hreflang="${code}" href="${published(prefix(code) + page)}" />`),
          `<link rel="alternate" hreflang="x-default" href="${published(page)}" />`,
        ].join("\n");
        const labels = { en: "WebDAV guide", "zh-TW": "WebDAV 連線指南", "zh-CN": "WebDAV 连接指南", ja: "WebDAV 接続ガイド" };
        const menu = languages
          .map((code) => `<a href="${published(prefix(code) + page)}" hreflang="${code}" lang="${code}"${code === language ? ' aria-current="true"' : ""}>${code}</a>`)
          .join("");
        put(
          file,
          `<html lang="${language}"><head><title>${labels[language]}</title><meta name="description" content="${language} description" />${alternate}</head><body><header class="site-header"><details class="lang-menu">${menu}</details></header><main id="content"><h1>${labels[language]}</h1><p>${labels[language]} introduction</p><h2 id="address">${labels[language]} address</h2><p><a href="${published(prefix(language) + "docs/webdav.html")}">${labels[language]}</a></p><pre><code>rclone config</code></pre></main></body></html>`,
        );
      }
    }
    change({
      root: temporary,
      put,
      edit(file, transform) {
        put(file, transform(readFileSync(join(temporary, file), "utf8")));
      },
    });
    const result = spawnSync(process.execPath, [checker, "--site", temporary], { encoding: "utf8" });
    if (expected) {
      assert.equal(result.status, 1);
      assert.match(result.stderr, expected);
    } else assert.equal(result.status, 0, result.stderr);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

test("complete same-language pages and reciprocal language switches pass", () => fixture(() => {}));
test("missing localized guide fails", () => fixture(({ root }) => unlinkSync(join(root, "ja/docs/webdav.html")), /missing: every page/));
test("body links cannot silently switch to English even when the same URL is a language alternative", () =>
  fixture(({ edit }) => edit("zh-TW/docs/webdav.html", (html) => html.replace(/<p><a href="[^"]+"/, `<p><a href="${base}docs/webdav.html"`)), /local reading link changes language/));
test("translated guides keep section IDs for deep-link language switching", () =>
  fixture(({ edit }) => edit("zh-TW/docs/webdav.html", (html) => html.replace('id="address"', 'id="different"')), /missing source section anchors/));
test("localized command examples cannot drift from the source", () =>
  fixture(({ edit }) => edit("zh-CN/docs/webdav.html", (html) => html.replace("rclone config", "rclone config wrong")), /changes a command/));
test("a summary cannot silently omit source paragraphs", () =>
  fixture(({ edit }) => edit("ja/docs/webdav.html", (html) => html.replace(/<p>[^<]+<\/p>/, "")), /different number of p elements/));
test("language switches must preserve the guide instead of returning home", () =>
  fixture(({ edit }) => edit("docs/webdav.html", (html) => html.replace(`${base}ja/docs/webdav.html" hreflang`, `${base}ja/" hreflang`)), /language switch should link/));
test("duplicate IDs fail", () => fixture(({ edit }) => edit("docs/webdav.html", (html) => html.replace("</main>", '<div id="address"></div></main>')), /duplicate element IDs/));
test("new English-only pages cannot bypass the translation list", () =>
  fixture(({ put }) => put("docs/extra.html", '<html lang="en"><head><title>Extra</title><meta name="description" content="Extra" /></head><body></body></html>'), /every published page/));
