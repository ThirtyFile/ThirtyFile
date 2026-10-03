// The Mac style (components/style/mac), chosen in the account: its sidebar, toolbar and path bar; every key of its map;
// Quick look; folders that expand in place in the List view; dragging onto the path bar; and search. Each test signs in
// as an account of its own, so the other tests keep the Windows style.
import { expect, test, type Page } from "@playwright/test";
import { answer, listing, makeFolder, openFolder, signInAsNewUser, uploadFile } from "./helpers";

/** Signed in as a new account that chose the Mac style, at a computer's width */
async function macStyle(page: Page, view?: "grid" | "list" | "columns") {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signInAsNewUser(page, "mac");
  expect((await page.request.put("/api/auth/style", { data: { style: "mac" } })).ok()).toBe(true);
  if (view) await page.addInitScript((v) => localStorage.setItem("tf-view", JSON.stringify(v)), view);
}

/** An item of the list, in any view */
const item = (page: Page, name: string) => page.locator("[data-node-id]").filter({ has: page.locator("[data-name]", { hasText: new RegExp(`^${name}$`) }) });
const selected = (page: Page) => page.locator('[data-node-id][aria-selected="true"] [data-name]').allTextContents();
const nameOf = async (page: Page, id: string): Promise<string> => (await (await page.request.get(`/api/nodes/${id}`)).json()).node.name;
const childNames = async (page: Page, id: string): Promise<string[]> =>
  ((await (await page.request.get(`/api/nodes/${id}/children`)).json()) as { name: string }[]).map((n) => n.name).sort();
const pathBar = (page: Page) => page.getByRole("navigation", { name: "Path bar" });

test("the window: the sidebar in groups, the toolbar, the path bar; ⌘↓ opens, ⌘↑ goes up, ⌘[ and ⌘] go back and forward, ⌘⇧G goes to a folder", async ({ page }) => {
  await macStyle(page);
  const top = await makeFolder(page, "Mac");
  const topName = await nameOf(page, top);
  const reports = await makeFolder(page, "Reports", top);
  await uploadFile(page, reports, "q1.txt", "one");
  await openFolder(page, top);

  // The sidebar's groups and the toolbar's controls; no address bar to type in
  const sidebar = page.getByRole("navigation", { name: "File locations" });
  for (const group of ["Favorites", "Spaces", "Shared", "Tags"]) await expect(sidebar.getByRole("heading", { name: group })).toBeVisible();
  await expect(sidebar.getByRole("link", { name: "Recent" })).toBeVisible();
  await expect(sidebar.getByRole("link", { name: "Trash" })).toBeVisible();
  const views = page.getByRole("group", { name: "View" });
  await expect(views.getByRole("button", { name: "Icons" })).toHaveAttribute("aria-pressed", "true");
  await expect(views.getByRole("button", { name: "List" })).toBeVisible();
  await expect(views.getByRole("button", { name: "Columns" })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Full path" })).toHaveCount(0);
  await expect(pathBar(page)).toContainText(topName);

  // ⌘↓ opens the folder selected; the path bar follows
  await item(page, "Reports").click();
  await page.keyboard.press("ControlOrMeta+ArrowDown");
  await page.waitForURL(`**/files/${reports}`);
  await expect(pathBar(page).getByText("Reports", { exact: true })).toHaveAttribute("aria-current", "page");
  await expect(item(page, "q1.txt")).toBeVisible();
  // ⌘↑ goes to the folder above, with the folder came from selected
  await page.keyboard.press("ControlOrMeta+ArrowUp");
  await page.waitForURL(`**/files/${top}`);
  await expect.poll(() => selected(page)).toEqual(["Reports"]);
  // ⌘[ and ⌘] go back and forward
  await page.keyboard.press("ControlOrMeta+BracketLeft");
  await page.waitForURL(`**/files/${reports}`);
  await page.keyboard.press("ControlOrMeta+BracketRight");
  await page.waitForURL(`**/files/${top}`);
  // A part of the path bar opens its folder
  await page.goto(`/files/${reports}`);
  await pathBar(page).getByRole("link", { name: topName }).click();
  await page.waitForURL(`**/files/${top}`);
  await expect(pathBar(page).getByText(topName, { exact: true })).toHaveAttribute("aria-current", "page");

  // ⌘⇧G: Go to folder, with the path of the folder open
  await page.keyboard.press("ControlOrMeta+Shift+KeyG");
  const dialog = page.getByRole("dialog", { name: "Go to folder" });
  const box = dialog.getByRole("textbox", { name: "Full path" });
  await expect(box).toHaveValue(`/My files/${topName}`);
  await box.fill(`/My files/${topName}/Reports`);
  await box.press("Enter");
  await page.waitForURL(`**/files/${reports}`);
  await expect(dialog).toHaveCount(0);
});

test("Enter renames, ⌘⌥N makes a folder in its sorted place, ⌘I shows details, ⌘⌫ moves to the trash and ⌘Z undoes it, ⌘⌥⌫ deletes for good after asking", async ({ page }) => {
  await macStyle(page, "list");
  const top = await makeFolder(page, "Keys");
  await uploadFile(page, top, "alpha.txt", "a");
  await uploadFile(page, top, "omega.txt", "o");
  await openFolder(page, top);

  // Enter: rename, not open
  await item(page, "alpha.txt").click();
  await page.keyboard.press("Enter");
  const renameBox = page.getByRole("textbox", { name: "New name" });
  await expect(renameBox).toBeFocused();
  await renameBox.fill("beta.txt");
  const renamed = answer(page, "PATCH", /\/api\/nodes\//);
  await renameBox.press("Enter");
  expect((await renamed).ok()).toBe(true);
  await expect(item(page, "beta.txt")).toBeVisible();
  expect(new URL(page.url()).pathname).toBe(`/files/${top}`);

  // ⌘⌥N: a new folder, renamed in its sorted place (folders first), not at the end
  const made = answer(page, "POST", "/api/folders");
  await page.keyboard.press("ControlOrMeta+Alt+KeyN");
  expect((await made).ok()).toBe(true);
  const newBox = page.getByRole("textbox", { name: "New name" });
  await expect(newBox).toBeFocused();
  await newBox.fill("Gamma");
  const named = answer(page, "PATCH", /\/api\/nodes\//);
  await newBox.press("Enter");
  expect((await named).ok()).toBe(true);
  await expect(page.locator("[data-node-id] [data-name]")).toHaveText(["Gamma", "beta.txt", "omega.txt"]);

  // ⌘I: the details of the item selected
  await item(page, "omega.txt").click();
  await page.keyboard.press("ControlOrMeta+KeyI");
  await expect(page.getByRole("complementary", { name: "Details pane" }).getByText("omega.txt").first()).toBeVisible();

  // ⌘⌫: to the trash, after the same question as in every style; ⌘Z puts it back
  await page.keyboard.press("ControlOrMeta+Backspace");
  const ask = page.getByRole("alertdialog").or(page.getByRole("dialog")).filter({ hasText: "Move 1 item to trash?" });
  const trashed = answer(page, "POST", "/api/nodes/trash");
  await ask.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  await expect(item(page, "omega.txt")).toHaveCount(0);
  const restored = answer(page, "POST", "/api/trash/restore");
  await page.keyboard.press("ControlOrMeta+KeyZ");
  expect((await restored).ok()).toBe(true);
  await expect(item(page, "omega.txt")).toBeVisible();

  // ⌘⌥⌫: delete for good, after asking (and nothing without the answer)
  await item(page, "omega.txt").click();
  await page.keyboard.press("ControlOrMeta+Alt+Backspace");
  const forGood = page.getByRole("alertdialog").or(page.getByRole("dialog")).filter({ hasText: "Permanently delete 1 item?" });
  await expect(forGood).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(forGood).toHaveCount(0);
  expect(await childNames(page, top)).toContain("omega.txt");

  // Esc clears the selection; "?" shows the Mac map
  await item(page, "beta.txt").click();
  await page.keyboard.press("Escape");
  await expect.poll(() => selected(page)).toEqual([]);
  await page.keyboard.press("Shift+Slash");
  const shortcuts = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  await expect(shortcuts).toContainText("Like a Mac.");
  await expect(shortcuts.getByRole("row", { name: "Enter Rename", exact: true })).toBeVisible();
});

test("⌘C, then ⌥⌘V moves here, through the conflict dialog when a name is taken; ⌘V copies; ⌘A selects all", async ({ page }) => {
  await macStyle(page, "list");
  const top = await makeFolder(page, "Move");
  const topName = await nameOf(page, top);
  const from = await makeFolder(page, "From", top);
  const to = await makeFolder(page, "To", top);
  await uploadFile(page, from, "move.txt", "m");
  await uploadFile(page, from, "copy.txt", "c");
  await uploadFile(page, to, "copy.txt", "already here");
  await openFolder(page, from);
  /** To a folder next to this one, the way people go (the clipboard is the page's) */
  const goTo = async (name: string, id: string) => {
    await pathBar(page).getByRole("link", { name: topName }).click();
    await page.waitForURL(`**/files/${top}`);
    await item(page, name).dblclick();
    await page.waitForURL(`**/files/${id}`);
    await expect(pathBar(page).getByText(name, { exact: true })).toHaveAttribute("aria-current", "page");
  };

  // ⌘C, then ⌥⌘V in another folder: moved, not copied
  await item(page, "move.txt").click();
  await page.keyboard.press("ControlOrMeta+KeyC");
  await goTo("To", to);
  const moved = answer(page, "POST", "/api/nodes/move");
  await page.keyboard.press("ControlOrMeta+Alt+KeyV");
  expect((await moved).ok()).toBe(true);
  await expect(item(page, "move.txt")).toBeVisible();
  expect(await childNames(page, from)).toEqual(["copy.txt"]);

  // A name the folder already has goes through the conflict dialog, as in every style
  await goTo("From", from);
  await item(page, "copy.txt").click();
  await page.keyboard.press("ControlOrMeta+KeyC");
  await goTo("To", to);
  await page.keyboard.press("ControlOrMeta+Alt+KeyV");
  const conflict = page.getByRole("dialog").filter({ hasText: 'The destination already has a file named "copy.txt"' });
  await expect(conflict).toBeVisible();
  const movedToo = answer(page, "POST", "/api/nodes/move");
  await conflict.getByRole("button", { name: /Keep both/ }).click();
  expect((await movedToo).ok()).toBe(true);
  await expect.poll(() => childNames(page, to)).toEqual(["copy (1).txt", "copy.txt", "move.txt"]);
  expect(await childNames(page, from)).toEqual([]);

  // ⌘A: everything
  await item(page, "move.txt").click();
  await page.keyboard.press("ControlOrMeta+KeyA");
  await expect.poll(() => selected(page)).toHaveLength(3);

  // ⌘C, then ⌘V: copied
  await item(page, "move.txt").click();
  await page.keyboard.press("ControlOrMeta+KeyC");
  await goTo("From", from);
  const copied = answer(page, "POST", "/api/nodes/copy");
  await page.keyboard.press("ControlOrMeta+KeyV");
  expect((await copied).ok()).toBe(true);
  await expect.poll(() => childNames(page, from)).toEqual(["move.txt"]);
  expect(await childNames(page, to)).toContain("move.txt");
});

test("Space opens Quick look; the arrows go to the next item; Space closes it", async ({ page }) => {
  await macStyle(page, "list");
  const top = await makeFolder(page, "Look");
  await uploadFile(page, top, "a-first.txt", "first file's text");
  await uploadFile(page, top, "b-second.txt", "second file's text");
  await openFolder(page, top);

  await item(page, "a-first.txt").click();
  await page.keyboard.press("Space");
  const look = page.getByRole("dialog", { name: "Quick look: a-first.txt" });
  await expect(look).toContainText("first file's text");
  await expect(look).toContainText("1 of 2");
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("dialog", { name: "Quick look: b-second.txt" })).toContainText("second file's text");
  // The list follows
  await expect.poll(() => selected(page)).toEqual(["b-second.txt"]);
  await page.keyboard.press("Space");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(item(page, "b-second.txt")).toBeFocused();
});

test("in the List view, → and ← expand and collapse a folder in place, and its triangle does too", async ({ page }) => {
  await macStyle(page, "list");
  const top = await makeFolder(page, "Tree");
  const inner = await makeFolder(page, "Inner", top);
  await uploadFile(page, inner, "deep.txt", "deep");
  await uploadFile(page, top, "top.txt", "top");
  await openFolder(page, top);

  await expect(page.getByRole("treegrid")).toBeVisible();
  await item(page, "Inner").click();
  const loaded = listing(page, inner);
  await page.keyboard.press("ArrowRight");
  await loaded;
  await expect(item(page, "Inner")).toHaveAttribute("aria-expanded", "true");
  await expect(item(page, "deep.txt")).toHaveAttribute("aria-level", "2");
  // → again goes into it; ← back to the folder, then collapses it
  await page.keyboard.press("ArrowRight");
  await expect(item(page, "deep.txt")).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(item(page, "Inner")).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(item(page, "deep.txt")).toHaveCount(0);
  // The triangle
  await item(page, "Inner").locator("[data-disclosure]").click();
  await expect(item(page, "deep.txt")).toBeVisible();
  await item(page, "Inner").locator("[data-disclosure]").click();
  await expect(item(page, "deep.txt")).toHaveCount(0);
});

test("Shift+F10 opens the menu in Finder's order, F5 refreshes, and in Columns → and ← go between columns", async ({ page }) => {
  await macStyle(page, "columns");
  const top = await makeFolder(page, "Menu");
  const topName = await nameOf(page, top);
  const inner = await makeFolder(page, "Inner", top);
  await uploadFile(page, inner, "inside.txt", "i");
  await openFolder(page, top);

  await item(page, "Inner").click();
  await page.keyboard.press("Shift+F10");
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem")).toHaveText([
    /Open/,
    /Open in new tab/,
    /Quick look/,
    /Move to trash/,
    /Get info/,
    /Rename/,
    /Compress/,
    /Download/,
    /Copy/,
    /Move to/,
    /Copy to/,
    /Share with/,
    /Create share link/,
    /favorites/,
    /Tags/,
  ]);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);

  const refreshed = listing(page, top);
  await page.keyboard.press("F5");
  expect((await refreshed).ok()).toBe(true);

  await item(page, "Inner").focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForURL(`**/files/${inner}`);
  await expect(page.getByRole("listbox", { name: "Inner" }).getByRole("option", { name: "inside.txt" })).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await page.waitForURL(`**/files/${top}`);
  await expect(page.getByRole("listbox", { name: topName }).getByRole("option", { name: "Inner" })).toBeFocused();
});

test("on a phone, the Mac style has the layout every style shares", async ({ page }) => {
  await macStyle(page);
  await page.setViewportSize({ width: 375, height: 812 });
  const top = await makeFolder(page, "Phone");
  await openFolder(page, top);
  await expect(page.getByRole("button", { name: "Navigation pane" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "File path" })).toBeVisible();
  await expect(pathBar(page)).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
});

test("items dragged onto a folder of the path bar move there", async ({ page }) => {
  await macStyle(page, "list");
  const top = await makeFolder(page, "Drag");
  const topName = await nameOf(page, top);
  const sub = await makeFolder(page, "Sub", top);
  await uploadFile(page, sub, "dragged.txt", "d");
  await openFolder(page, sub);

  const moved = answer(page, "POST", "/api/nodes/move");
  await item(page, "dragged.txt")
    .locator("[data-drag-handle]")
    .dragTo(pathBar(page).getByRole("link", { name: topName }));
  expect((await moved).ok()).toBe(true);
  await expect(item(page, "dragged.txt")).toHaveCount(0);
  expect(await childNames(page, top)).toEqual(["Sub", "dragged.txt"]);
});

test("⌘F goes to the search box, which finds items in the folder", async ({ page }) => {
  await macStyle(page);
  const top = await makeFolder(page, "Find");
  const sub = await makeFolder(page, "Deeper", top);
  await uploadFile(page, sub, "needle-in-mac.txt", "n");
  await openFolder(page, top);

  await page.keyboard.press("ControlOrMeta+KeyF");
  const box = page.getByRole("textbox", { name: /^Search/ });
  await expect(box).toBeFocused();
  await box.fill("needle-in-mac");
  await page.waitForURL(/\/search\?q=needle-in-mac/);
  await expect(item(page, "needle-in-mac.txt")).toBeVisible();
});
