// A large folder loads the parts in view, not everything: the keyboard still reaches every item, and Select all and
// Shift select items that aren't loaded, which the changes then include.
import { expect, test, type Page, type Response } from "@playwright/test";
import { answer, listing, makeFolder, signIn } from "./helpers";

/** More than two parts of 500: the part at the end isn't loaded when the folder opens */
const COUNT = 1200;
const nameOf = (i: number) => `Item ${String(i).padStart(4, "0")}`;

/** A folder of `size` folders, made through the API a few at a time; in `parent`, or in My files */
async function bigFolder(page: Page, size = COUNT, parent?: string): Promise<string> {
  const big = await makeFolder(page, "Large", parent);
  for (let i = 0; i < size; i += 25) await Promise.all(Array.from({ length: Math.min(25, size - i) }, (_, k) => makeFolder(page, nameOf(i + k), big)));
  return big;
}

const count = async (page: Page, id: string) => (await (await page.request.get(`/api/nodes/${id}/children`)).json()).length;

/** The server's answer to the page asking for the part of `id` that starts at `offset` */
function part(page: Page, id: string, offset: number) {
  return page.waitForResponse((r) => {
    const url = new URL(r.url());
    return url.pathname === `/api/nodes/${id}/children` && url.searchParams.get("offset") === String(offset);
  });
}

/**
 * The server's answer to the last change of `count` items (`path`: the change's request). The page changes them a batch
 * of at most 1,000 at a time: a large change takes a while, and each batch may wait its turn behind other tests' changes.
 */
function lastBatch(page: Page, path: string, count: number): Promise<Response> {
  let left = count;
  return page.waitForResponse((r) => r.request().method() === "POST" && new URL(r.url()).pathname === path && (left -= r.request().postDataJSON().ids.length) <= 0);
}

/** Where the parts the page asks for start */
function parts(page: Page) {
  const offsets: number[] = [];
  page.on("request", (r) => {
    const url = new URL(r.url());
    if (url.pathname.endsWith("/children") && url.searchParams.has("offset")) offsets.push(Number(url.searchParams.get("offset")));
  });
  return offsets;
}

test.describe.configure({ timeout: 120_000 });

test("a large folder loads the parts in view, and End and Home reach every item", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page);
  const offsets = parts(page);
  const second = part(page, big, 500);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await expect(row(nameOf(0))).toBeVisible();
  await expect(page.locator("footer")).toContainText(`${COUNT.toLocaleString("en-US")} items`);
  // The part in view and the next one, not the part at the end
  await second;
  expect(new Set(offsets)).toEqual(new Set([0, 500]));

  await row(nameOf(0)).click();
  await page.keyboard.press("End");
  const last = row(nameOf(COUNT - 1));
  await expect(last).toHaveAttribute("aria-selected", "true");
  await expect(last).toBeFocused();
  expect(offsets).toContain(1000);
  await page.keyboard.press("ArrowUp");
  await expect(row(nameOf(COUNT - 2))).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Home");
  await expect(row(nameOf(0))).toHaveAttribute("aria-selected", "true");
  await expect(row(nameOf(0))).toBeFocused();

  // A new folder sorts after every "Item": the list goes there and its name can be typed at once
  await page.keyboard.press("Control+Shift+N");
  const box = page.getByRole("textbox", { name: /name/i });
  await expect(box).toBeFocused();
  await page.keyboard.type("Made at the end");
  // Named once the server has made the folder and renamed it: it may wait its turn behind other tests' changes
  const renamed = answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);
  await page.keyboard.press("Enter");
  expect((await renamed).ok()).toBe(true);
  await expect(row("Made at the end")).toBeVisible();
});

test("Select all takes the items not loaded too, less those left out, and the trash gets them all", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await row(nameOf(0)).click();
  await page.keyboard.press("Control+a");
  await expect(page.locator("footer")).toContainText(`${COUNT.toLocaleString("en-US")} items selected`);
  await expect(row(nameOf(3))).toHaveAttribute("aria-selected", "true");
  // Left out with Ctrl
  await row(nameOf(3)).click({ modifiers: ["Control"] });
  await expect(row(nameOf(3))).toHaveAttribute("aria-selected", "false");
  await expect(page.locator("footer")).toContainText(`${(COUNT - 1).toLocaleString("en-US")} items selected`);
  await page.keyboard.press("Delete");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(`Move ${(COUNT - 1).toLocaleString("en-US")} items to trash?`);
  const trashed = lastBatch(page, "/api/nodes/trash", COUNT - 1);
  await dialog.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  expect(await count(page, big)).toBe(1);
  await expect(row(nameOf(3))).toBeVisible();
  await expect(page.locator("footer")).toContainText(/^1 item(?!s)/);
});

test("Shift selects up to an item across parts not loaded, and a cut and paste moves all of them", async ({ page }) => {
  await signIn(page);
  // Both in a folder of their own: going up shows it, and My files holds the other tests' folders too
  const top = await makeFolder(page, "Large moves");
  const big = await bigFolder(page, COUNT, top);
  const other = await makeFolder(page, "Destination", top);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await row(nameOf(2)).click();
  await page.keyboard.press("Shift+End");
  await expect(row(nameOf(COUNT - 1))).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("footer")).toContainText(`${(COUNT - 2).toLocaleString("en-US")} items selected`);
  await page.keyboard.press("Control+x");
  // Within the page (the clipboard is the page's own): up to My files, and into the destination
  await page.keyboard.press("Alt+ArrowUp");
  await page.locator("[data-node-id]").filter({ hasText: "Destination" }).dblclick();
  await page.waitForURL(`**/files/${other}`);
  const moved = lastBatch(page, "/api/nodes/move", COUNT - 2);
  await page.getByRole("button", { name: "Paste" }).click();
  expect((await moved).ok()).toBe(true);
  expect(await count(page, other)).toBe(COUNT - 2);
  expect(await count(page, big)).toBe(2);
});

test("going up to a folder far down a large folder shows and selects the folder left", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page);
  const last = await makeFolder(page, "zz last", big);
  await page.goto(`/files/${big}`);
  await expect(page.locator("[data-node-id]").first()).toBeVisible();
  // Into the last item (by its address: it isn't loaded), then up again
  await page.goto(`/files/${last}`);
  await expect(page.getByText("No files here yet")).toBeVisible();
  await page.keyboard.press("Alt+ArrowUp");
  await page.waitForURL(`**/files/${big}`);
  const left = page.locator("[data-node-id]").filter({ hasText: "zz last" });
  await expect(left).toHaveAttribute("aria-selected", "true");
  await expect(left).toBeInViewport();
});

/** Parts of 500, with the end far enough that a part in the middle isn't loaded when the list goes there */
const SPANNED = 2100;

test("Ctrl+Shift across parts not loaded counts the item it starts from once, and the trash gets them all", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page, SPANNED);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await row(nameOf(0)).click();
  // Ctrl moves only the focus
  await page.keyboard.press("Control+End");
  const last = row(nameOf(SPANNED - 1));
  await expect(last).toBeFocused();
  await last.click({ modifiers: ["Control", "Shift"] });
  await expect(page.locator("footer")).toContainText(`${SPANNED.toLocaleString("en-US")} items selected`);
  await page.keyboard.press("Delete");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText(`Move ${SPANNED.toLocaleString("en-US")} items to trash?`);
  const trashed = lastBatch(page, "/api/nodes/trash", SPANNED);
  await dialog.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  await expect(page.getByText(/Moved 2,?100 items to trash/)).toBeVisible();
  expect(await count(page, big)).toBe(0);
});

test("a span of exactly 2,000 items is changed in two batches and ends there", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page, SPANNED);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await row(nameOf(0)).click();
  for (let i = 0; i < 100; i++) await page.keyboard.press("ArrowDown");
  await expect(row(nameOf(100))).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Shift+End");
  await expect(row(nameOf(SPANNED - 1))).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("footer")).toContainText("2,000 items selected");
  await page.keyboard.press("Delete");
  const asked: string[] = [];
  page.on("request", (r) => r.url().endsWith(`/nodes/${big}/select`) && asked.push(r.url()));
  const trashed = lastBatch(page, "/api/nodes/trash", 2000);
  await page.getByRole("dialog").getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  // Not an error after the last batch, when the span's last item is already in the trash
  await expect(page.getByText(/Moved 2,?000 items to trash/)).toBeVisible();
  expect(asked).toHaveLength(2);
  expect(await count(page, big)).toBe(100);
});

test("a new folder made with the end not loaded shows after the items loaded, and stays there after renaming", async ({ page }) => {
  await signIn(page);
  const big = await bigFolder(page);
  const offsets = parts(page);
  const second = part(page, big, 500);
  await page.goto(`/files/${big}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await expect(row(nameOf(0))).toBeVisible();
  await second;
  expect(offsets).not.toContain(1000);

  // "New folder" sorts after every "Item", in the part not loaded: it shows after the last item loaded instead
  await page.keyboard.press("Control+Shift+N");
  const box = page.getByRole("textbox", { name: "New name" });
  await expect(box).toBeFocused();
  await box.fill("Made here");
  // Renamed once the server has answered, and the list loads again
  const renamed = answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);
  const listed = listing(page, big, renamed);
  await box.press("Enter");
  await expect(row("Made here")).toBeVisible();
  expect((await renamed).ok()).toBe(true);
  await listed;
  const next = () =>
    page.locator("[data-node-id]").evaluateAll((rows, name) => {
      const at = rows.findIndex((r) => r.textContent?.includes(name));
      return rows[at + 1]?.querySelector("[data-name]")?.textContent;
    }, nameOf(999));
  await expect.poll(next).toBe("Made here");
  await expect(row("Made here")).toHaveAttribute("aria-selected", "true");
});
