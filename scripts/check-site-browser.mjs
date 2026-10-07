// Rendered documentation checks. Needs the existing web Playwright dependency and its Chromium browser.
// Usage: node scripts/check-site-browser.mjs <scratch screenshot directory>
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdirSync, readFileSync, statSync } from "node:fs";
import { extname, join, resolve, sep } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const site = resolve(root, "site");
const output = process.argv[2] && resolve(process.argv[2]);
if (!output || output === site || output.startsWith(site + sep)) throw new Error("Use a scratch directory outside site for screenshots");
mkdirSync(output, { recursive: true });
const require = createRequire(join(root, "web/package.json"));
const { chromium } = require("@playwright/test");
const source = readFileSync(join(site, "assets/site.js"), "utf8");
const pages = JSON.parse(source.match(/^\s*const TRANSLATED = (\[.*\]);$/m)[1]);
const languages = ["en", "zh-TW", "zh-CN", "ja"];
const types = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".svg": "image/svg+xml", ".webp": "image/webp" };
const server = createServer((request, response) => {
  try {
    let path = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    if (!path.startsWith("/ThirtyFile/")) {
      response.writeHead(404).end();
      return;
    }
    path = resolve(site, path.slice("/ThirtyFile/".length));
    if (path !== site && !path.startsWith(site + sep)) {
      response.writeHead(404).end();
      return;
    }
    if (statSync(path).isDirectory()) path = join(path, "index.html");
    response.writeHead(200, { "Content-Type": types[extname(path)] || "application/octet-stream" }).end(readFileSync(path));
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const base = `http://127.0.0.1:${server.address().port}/ThirtyFile/`;
let browser;
const errors = [];
const externalErrors = [];
let checked = 0;
const folder = (language) => (language === "en" ? "" : `${language}/`);

function observe(page) {
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (!["error", "warning"].includes(message.type())) return;
    const location = message.location().url;
    (location && !location.startsWith(base) ? externalErrors : errors).push(`${location}: ${message.text()}`);
  });
  page.on("requestfailed", (request) => {
    if (request.url().startsWith(base)) errors.push(`${request.url()}: ${request.failure()?.errorText}`);
  });
}

try {
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 }, locale: "en-US", permissions: ["clipboard-write"] });
  const page = await context.newPage();
  observe(page);
  for (const language of languages) {
    for (const inner of pages) {
      const response = await page.goto(base + folder(language) + inner, { waitUntil: "load" });
      assert.equal(response.status(), 200);
      assert.equal(await page.locator("html").getAttribute("lang"), language);
      assert.match(await page.title(), /ThirtyFile/);
      assert.ok((await page.locator("main h1").innerText()).length > 0);
      assert.ok(await page.locator("main h1").isVisible());
      assert.equal(await page.locator("vite-error-overlay, nextjs-portal").count(), 0);
      if (inner.startsWith("docs/")) {
        assert.equal(await page.locator(".docs-nav a").count(), 16);
        const sections = await page.locator("main h2[id]").count();
        assert.equal(await page.locator(".guide-toc").count(), sections >= 4 ? 1 : 0, `${language}/${inner}: section navigation`);
        assert.equal(await page.locator("nav.next").count(), 1);
        const hrefs = await page.locator(".docs-nav a, nav.next a, .site-footer a").evaluateAll((links) => links.map((link) => link.href));
        assert.ok(hrefs.filter((href) => href.startsWith(base)).every((href) => href.startsWith(base + folder(language)) && (language !== "en" || !/\/(zh-TW|zh-CN|ja)\//.test(href))));
      }
      checked++;
    }
  }

  // The reported flow: a Chinese installation guide must open the WebDAV article in Chinese.
  await page.goto(`${base}zh-TW/docs/index.html`);
  await page.locator('.docs-nav a[href$="/webdav.html"], .docs-nav a[href="webdav.html"]').click();
  assert.equal(await page.locator("html").getAttribute("lang"), "zh-TW");
  assert.ok(page.url().endsWith("/zh-TW/docs/webdav.html"));
  await page.locator(".guide-toc summary").click();
  await page.locator('.guide-toc a[href="#rclone"]').click();
  for (const language of ["zh-CN", "ja", "en", "zh-TW"]) {
    await page.locator(".lang-menu summary").click();
    await page.locator(`.lang-menu a[hreflang="${language}"]`).click();
    assert.equal(await page.locator("html").getAttribute("lang"), language);
    assert.ok(page.url().endsWith(`/${folder(language)}docs/webdav.html#rclone`));
  }
  await page.locator(".lang-menu summary").click();
  await page.keyboard.press("Escape");
  assert.equal(await page.locator(".lang-menu").getAttribute("open"), null);
  assert.equal(await page.locator(".lang-menu summary").evaluate((element) => element === document.activeElement), true);
  await page.locator(".theme-toggle").click();
  assert.ok(["light", "dark"].includes(await page.locator("html").getAttribute("data-theme")));
  const copy = page.locator(".copy").first();
  await copy.click();
  await copy.filter({ hasText: "已複製" }).waitFor();
  await page.goto(`${base}zh-TW/docs/webdav.html`);
  await page.screenshot({ path: join(output, "webdav-zh-TW-desktop.png"), fullPage: false });
  await page.goto(`${base}ja/docs/webdav.html`);
  await page.screenshot({ path: join(output, "webdav-ja-desktop.png"), fullPage: false });
  await context.close();

  for (const language of languages) {
    const mobile = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: "en-US" });
    const view = await mobile.newPage();
    observe(view);
    for (const inner of ["docs/index.html", "docs/webdav.html", "docs/storage.html", "docs/files.html"]) {
      await view.goto(base + folder(language) + inner);
      const width = await view.evaluate(() => ({ page: document.documentElement.scrollWidth, viewport: innerWidth }));
      assert.ok(width.page <= width.viewport + 1, `Horizontal overflow: ${language}/${inner} (${width.page})`);
      assert.ok(await view.locator("main h1").isVisible());
    }
    await view.goto(base + folder(language) + "docs/webdav.html");
    await view.locator(".guide-toc summary").click();
    await view.locator('.guide-toc a[href="#rclone"]').click();
    assert.ok(view.url().endsWith("#rclone"));
    if (language === "zh-CN") {
      await view.goto(base + "zh-CN/docs/webdav.html");
      await view.screenshot({ path: join(output, "webdav-zh-CN-mobile.png"), fullPage: false });
    }
    await mobile.close();
  }

  for (const language of languages) {
    const noScript = await browser.newContext({ javaScriptEnabled: false, reducedMotion: "reduce", viewport: { width: 1280, height: 720 } });
    const view = await noScript.newPage();
    await view.goto(base + folder(language) + "docs/index.html");
    await view.locator('.docs-nav a[href="webdav.html"]').click();
    assert.equal(await view.locator("html").getAttribute("lang"), language);
    await view.locator(".guide-toc summary").click();
    await view.locator('.guide-toc a[href="#windows"]').click();
    assert.ok(view.url().endsWith("#windows"));
    await view.locator("nav.next a.forward").click();
    assert.ok(view.url().endsWith(`/${folder(language)}docs/users.html`));
    assert.equal(await view.locator("html").getAttribute("lang"), language);
    await noScript.close();
  }

  const blockedStorage = await browser.newContext({ locale: "zh-TW" });
  await blockedStorage.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      get() {
        throw new Error("Storage disabled for regression test");
      },
    });
  });
  const blocked = await blockedStorage.newPage();
  observe(blocked);
  await blocked.goto(base + "zh-TW/docs/webdav.html");
  await blocked.locator(".theme-toggle").click();
  await blocked.locator("nav.next a.forward").click();
  assert.equal(await blocked.locator("html").getAttribute("lang"), "zh-TW");
  await blockedStorage.close();
  assert.deepEqual(errors, [], "Local scripts or assets produced browser errors");
  console.log(
    JSON.stringify(
      {
        result: "passed",
        pages: checked,
        desktop: "1280x720",
        mobile: "390x844",
        sameLanguageNavigation: true,
        sectionPreservingLanguageSwitch: true,
        noJavaScriptNavigation: true,
        blockedStorage: true,
        localConsoleErrors: errors,
        externalConsoleWarnings: [...new Set(externalErrors)],
        screenshots: output,
      },
      null,
      2,
    ),
  );
} finally {
  await browser?.close();
  await new Promise((done) => server.close(done));
}
