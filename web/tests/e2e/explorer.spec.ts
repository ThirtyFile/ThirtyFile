// File explorer against the real server: name conflicts when uploading, keyboard browsing, and search typed through an
// input method. Each test works in a folder of its own inside My files.
import { randomBytes } from "node:crypto";
import { expect, test, type Page } from "@playwright/test";
import { signIn, uploadFinished } from "./helpers";

interface Item {
  id: string;
  name: string;
  kind: string;
}

/**
 * A new folder, made through the API with the page's session. In My files (when no parent is given) its name gets a
 * random ending, so a test run again on the same server (a retry), or by another worker at the same moment, starts
 * afresh.
 */
async function folder(page: Page, name: string, parent?: string): Promise<string> {
  const parentId = parent ?? (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: parentId, name: parent ? name : `${name} ${randomBytes(4).toString("hex")}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

async function children(page: Page, id: string): Promise<Item[]> {
  return (await page.request.get(`/api/nodes/${id}/children`)).json();
}

async function content(page: Page, id: string) {
  return (await page.request.get(`/api/files/${id}/content`)).text();
}

test("uploading a name the folder already has asks what to do, and each choice does it", async ({ page }) => {
  // Three uploads, each a few changes on the server: behind a large change made by another test at the same time, each
  // of them can wait half a minute for its turn
  test.setTimeout(120_000);
  await signIn(page);
  const dir = await folder(page, "Conflicts");
  await page.goto(`/files/${dir}`);
  const upload = (text: string) => page.locator('input[type="file"][multiple]').setInputFiles([{ name: "note.txt", mimeType: "text/plain", buffer: Buffer.from(text) }]);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  const dialog = page.getByRole("dialog");

  let finished = uploadFinished(page);
  await upload("first");
  await finished;
  await expect(row("note.txt")).toBeVisible();
  const [original] = await children(page, dir);

  // Keep both: the new file gets a numbered name, the original stays as it was
  await upload("second");
  await expect(dialog.getByText('The destination already has a file named "note.txt"')).toBeVisible();
  finished = uploadFinished(page);
  await dialog.getByRole("button", { name: /Keep both/ }).click();
  await finished;
  await expect(row("note (1).txt")).toBeVisible();
  let items = await children(page, dir);
  expect(items.map((n) => n.name).sort()).toEqual(["note (1).txt", "note.txt"]);
  expect(await content(page, original.id)).toBe("first");
  expect(await content(page, items.find((n) => n.name === "note (1).txt")!.id)).toBe("second");

  // Skip, and Cancel: nothing is uploaded
  for (const choice of [/Skip this file/, /^Cancel$/]) {
    await upload("not uploaded");
    await dialog.getByRole("button", { name: choice }).click();
    await expect(dialog).toHaveCount(0);
  }
  items = await children(page, dir);
  expect(items).toHaveLength(2);
  expect(await content(page, original.id)).toBe("first");

  // Replace: the original file gets the new content
  await upload("third");
  finished = uploadFinished(page);
  await dialog.getByRole("button", { name: /Replace the file in the destination/ }).click();
  await finished;
  expect(await content(page, original.id)).toBe("third");
  expect(await children(page, dir)).toHaveLength(2);
  await expect(page.getByText(/Something went wrong|useMe must be used/)).toHaveCount(0);
});

// Details, and Large icons
for (const view of ["list", "grid"]) {
  test(`after Enter opens a folder, the arrow keys go on in it (${view} view)`, async ({ page }) => {
    await signIn(page);
    await page.evaluate((v) => localStorage.setItem("tf-view", JSON.stringify(v)), view);
    const top = await folder(page, "Keyboard");
    const level1 = await folder(page, "Level 1 a", top);
    await folder(page, "Level 1 b", top);
    await folder(page, "Level 2 a", level1);
    await folder(page, "Level 2 b", level1);
    await page.goto(`/files/${top}`);
    const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
    const focused = () => page.evaluate(() => document.activeElement?.closest("[data-node-id]")?.textContent ?? null);

    await row("Level 1 a").click();
    await page.keyboard.press("Enter");
    await page.waitForURL(`**/files/${level1}`);
    // The first item has the focus, and nothing is selected yet
    await expect.poll(focused).toContain("Level 2 a");
    await expect(page.locator('[data-node-id][aria-selected="true"]')).toHaveCount(0);
    await page.keyboard.press("ArrowDown");
    await expect(row("Level 2 b")).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("ArrowUp");
    await expect(row("Level 2 a")).toHaveAttribute("aria-selected", "true");
    expect(await focused()).toContain("Level 2 a");

    // Back up, then in again: still no mouse
    await page.keyboard.press("Backspace");
    await page.waitForURL(`**/files/${top}`);
    await expect.poll(focused).toContain("Level 1 a");
    await page.keyboard.press("Enter");
    await page.waitForURL(`**/files/${level1}`);
    await expect.poll(focused).toContain("Level 2 a");
  });
}

test("search waits for an input method to finish composing", async ({ page }) => {
  await signIn(page);
  await page.goto("/files");
  const box = page.getByRole("textbox", { name: /Search/ }).first();
  await box.click();
  const cdp = await page.context().newCDPSession(page);

  // Composing Zhuyin: the box shows it, but nothing is searched however long the pause
  await cdp.send("Input.imeSetComposition", { text: "ㄓ", selectionStart: 1, selectionEnd: 1 });
  await cdp.send("Input.imeSetComposition", { text: "ㄓㄨ", selectionStart: 2, selectionEnd: 2 });
  await expect(box).toHaveValue("ㄓㄨ");
  await page.waitForTimeout(1000);
  expect(new URL(page.url()).pathname).toBe("/files");

  // Choosing the candidate commits the text, which is searched
  await cdp.send("Input.insertText", { text: "中" });
  await page.waitForURL(/\/search\?q=/);
  expect(new URL(page.url()).searchParams.get("q")).toBe("中");
});

test("Alt+Up goes up a folder also from a menu button, and a resize handle shows it has the focus", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signIn(page);
  const me = await (await page.request.get("/api/auth/me")).json();
  const sub = (await (await page.request.post("/api/folders", { data: { parent_id: me.root_id, name: `Up ${Date.now().toString(36)}` } })).json()).id;
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${sub}`);
  const newMenu = page.getByRole("button", { name: "New", exact: true });
  await newMenu.focus();
  await page.keyboard.press("Alt+ArrowUp");
  await page.waitForURL(`**/files/${me.root_id}`);
  await expect(page.getByRole("menu")).toHaveCount(0);

  // A column's resize handle, reached with Tab, is drawn
  const handle = page.getByRole("separator", { name: 'Resize the "Name" column' });
  await handle.focus();
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Tab");
  await expect(handle).toBeFocused();
  expect(await handle.evaluate((el) => getComputedStyle(el).backgroundColor)).not.toBe("rgba(0, 0, 0, 0)");
});
