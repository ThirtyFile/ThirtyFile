// axe-core on the pages that show an empty state: every list that can be empty, in each view, and the previews of
// Office files that can't be shown (empty, damaged, a legacy format), in the file's tab, the Mac style's Gallery and
// Quick look, and a share link's floating preview. In both styles and both themes. Each test signs in as an account of
// its own, so its lists are empty and the other tests keep their style.
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signInAsNewUser, unique, uploadFile } from "./helpers";

/**
 * What axe finds on the page: its violations, and the texts whose contrast it couldn't work out (a color it can't
 * read, such as an oklch() background) among `texts`: those must be on a background it can check
 */
async function axe(page: Page, texts: string[] = []): Promise<string[]> {
  const r = await new AxeBuilder({ page }).analyze();
  const found = r.violations.flatMap((v) => v.nodes.map((n) => `${v.id}: ${n.target.join(" ")}: ${n.failureSummary?.replace(/\s+/g, " ")}`));
  for (const v of r.incomplete.filter((v) => v.id === "color-contrast"))
    for (const n of v.nodes)
      if (texts.some((text) => n.html.includes(text)))
        found.push(
          `color-contrast not measurable: ${n.target.join(" ")}: ${[...n.any, ...n.all, ...n.none].map((a) => (a.data as { message?: string } | null)?.message ?? a.message).join(" ")}`,
        );
  return found;
}

/** What each list says when it is empty (in the Columns view, its column says the folder is empty) */
const EMPTY =
  /This folder is empty\.|Nothing has been shared with you yet|No recent files yet|No favorites yet|Trash is empty|You haven't deleted anything|Nothing has this tag yet|Nothing matches yet|No matching files found|No files here yet/;

/** Office files whose preview shows why they can't be shown */
const OFFICE: [name: string, content: string | Buffer, message: string][] = [
  ["empty.xlsx", "", "This file is empty."],
  ["empty.docx", "", "This file is empty."],
  ["empty.pptx", "", "This file is empty."],
  ["damaged.xlsx", "not a zip archive", "isn't a valid Office document"],
  ["legacy.xlsx", Buffer.from([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]), "legacy Office file"],
];

for (const style of ["windows", "mac"] as const)
  for (const theme of ["light", "dark"] as const) {
    /** Signed in as a new account in this style and theme, at a computer's width */
    const setUp = async (page: Page, prefix: string) => {
      await page.setViewportSize({ width: 1280, height: 720 });
      await page.addInitScript((theme) => localStorage.setItem("tf-theme", theme), theme);
      await signInAsNewUser(page, prefix);
      expect((await page.request.put("/api/auth/style", { data: { style } })).ok()).toBe(true);
    };

    test(`empty lists in every view pass axe (${style} style, ${theme} theme)`, async ({ page }) => {
      test.setTimeout(180_000);
      await setUp(page, "axe-lists");
      const tag = await (await page.request.post("/api/tags", { data: { name: `Empty ${unique()}`, color: "red" } })).json();
      const smart = await (await page.request.post("/api/smart-folders", { data: { name: `None ${unique()}`, query: { name: `none-${unique()}` } } })).json();
      const folder = await makeFolder(page, "Empty");
      const pages = ["/shared-with-me", "/recent", "/favorites", "/trash", `/tags/${tag.id}`, `/smart/${smart.id}`, `/search?q=none-${unique()}`, `/files/${folder}`];
      const views = style === "mac" ? ["grid", "list", "columns", "gallery"] : ["list", "grid", "columns"];
      const found: string[] = [];
      for (const view of views) {
        await page.evaluate((v) => localStorage.setItem("tf-view", JSON.stringify(v)), view);
        for (const url of pages) {
          await page.goto(url);
          await expect(page.getByText(EMPTY).first()).toBeVisible();
          found.push(...(await axe(page)).map((f) => `${view} ${url}: ${f}`));
        }
      }
      expect(found).toEqual([]);
    });

    test(`previews of Office files that can't be shown pass axe (${style} style, ${theme} theme)`, async ({ page, browser, baseURL }) => {
      test.setTimeout(180_000);
      await setUp(page, "axe-previews");
      const folder = await makeFolder(page, "Previews");
      const ids: string[] = [];
      for (const [name, content] of OFFICE) ids.push(await uploadFile(page, folder, name, content));
      const found: string[] = [];

      // In the file's own tab
      for (const [i, [name, , message]] of OFFICE.entries()) {
        await page.goto(`/view/${ids[i]}`);
        await expect(page.getByText(message)).toBeVisible();
        found.push(...(await axe(page, [message])).map((f) => `tab ${name}: ${f}`));
        if (!name.endsWith(".xlsx")) continue;
        // ...and in the spreadsheet editor, which says the same
        await page.getByRole("button", { name: "Edit workbook" }).click();
        await expect(page.getByRole("button", { name: "Back to preview" })).toBeVisible();
        await expect(page.getByText(message)).toBeVisible();
        found.push(...(await axe(page, [message])).map((f) => `editor ${name}: ${f}`));
      }

      if (style === "mac") {
        // The Gallery view's preview, and Quick look over it
        await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("gallery")));
        await page.goto(`/files/${folder}`);
        const strip = page.getByRole("listbox", { name: "Thumbnails" });
        for (const [name, , message] of OFFICE) {
          await strip.getByRole("option", { name, exact: true }).click();
          await expect(page.getByRole("region", { name: `Preview: ${name}` }).getByText(message)).toBeVisible();
          found.push(...(await axe(page, [message])).map((f) => `gallery ${name}: ${f}`));
          await page.keyboard.press("Space");
          const look = page.getByRole("dialog", { name: `Quick look: ${name}` });
          await expect(look.getByText(message)).toBeVisible();
          found.push(...(await axe(page, [message])).map((f) => `quick look ${name}: ${f}`));
          await page.keyboard.press("Space");
          await expect(look).toHaveCount(0);
        }
      }

      // A share link's floating preview, seen by a visitor in the same theme
      const res = await page.request.post("/api/shares", { data: { node_id: folder } });
      expect(res.ok()).toBe(true);
      const link = (await res.json()).id as string;
      const visitor = await browser.newContext({ baseURL, viewport: { width: 1280, height: 720 } });
      const guest = await visitor.newPage();
      await guest.addInitScript((theme) => localStorage.setItem("tf-theme", theme), theme);
      await guest.goto(`/share/${link}`);
      for (const [name, , message] of OFFICE) {
        await guest.getByText(name, { exact: true }).first().dblclick();
        const preview = guest.getByRole("dialog", { name });
        await expect(preview.getByText(message)).toBeVisible();
        found.push(...(await axe(guest, [message])).map((f) => `share link ${name}: ${f}`));
        await guest.keyboard.press("Escape");
        await expect(preview).toHaveCount(0);
      }
      await visitor.close();

      expect(found).toEqual([]);
    });
  }
