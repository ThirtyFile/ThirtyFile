// Click the name of the selected item to rename it, as in File Explorer (the Windows style): in the list and in the
// folder tree, and a double-click that opens without renaming
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

const row = (page: Page, name: string) => page.locator("[data-node-id]").filter({ hasText: name });
const nameOf = (page: Page, name: string) => row(page, name).locator("[data-name]");
const box = (page: Page) => page.getByRole("textbox", { name: "New name" });

/** Longer than a double-click, so the next click is a click of its own */
const pause = (page: Page) => page.waitForTimeout(700);

test("clicking the name of the selected item renames it; the click that selects it, its icon and Ctrl don't", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Click rename");
  await uploadFile(page, dir, "draft.txt", "words");
  await uploadFile(page, dir, "other.txt", "more words");
  await page.goto(`/files/${dir}`);

  // The click that selects it, then a click on the icon, then Ctrl+click: none of them renames
  await nameOf(page, "draft.txt").click();
  await pause(page);
  await expect(box(page)).toHaveCount(0);
  await row(page, "draft.txt").locator("svg").first().click();
  await pause(page);
  await nameOf(page, "draft.txt").click({ modifiers: ["Control"] });
  await nameOf(page, "draft.txt").click({ modifiers: ["Control"] });
  await pause(page);
  await expect(box(page)).toHaveCount(0);

  // A click on the name of the item selected: the name box after a moment, with the name before the extension chosen
  await nameOf(page, "draft.txt").click();
  await expect(box(page)).toBeFocused();
  await expect(box(page)).toHaveValue("draft.txt");
  await page.keyboard.type("final");
  await page.keyboard.press("Enter");
  await expect(row(page, "final.txt")).toBeVisible();
  const items: { name: string }[] = await (await page.request.get(`/api/nodes/${dir}/children`)).json();
  expect(items.map((n) => n.name).sort()).toEqual(["final.txt", "other.txt"]);

  // Clicking another item meanwhile cancels it
  await pause(page);
  await nameOf(page, "final.txt").click();
  await nameOf(page, "other.txt").click();
  await pause(page);
  await expect(box(page)).toHaveCount(0);
});

test("a double-click on the name of the selected folder opens it without renaming", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Double click");
  const inner = await makeFolder(page, "Inside", dir);
  await page.goto(`/files/${dir}`);
  await nameOf(page, "Inside").click();
  await pause(page);
  await nameOf(page, "Inside").dblclick();
  await page.waitForURL(`**/files/${inner}`);
  await pause(page);
  await expect(box(page)).toHaveCount(0);
  expect((await (await page.request.get(`/api/nodes/${inner}`)).json()).node.name).toBe("Inside");
});

test("clicking the open folder's name in the folder tree renames it there", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Tree rename");
  const inner = await makeFolder(page, "Old name", dir);
  await page.goto(`/files/${inner}`);
  const item = page.getByRole("treeitem", { name: "Old name" });
  await expect(item).toBeVisible();
  // The first click brings the focus to the tree; the next one, on the name, renames
  await item.locator("[data-name]").click();
  await pause(page);
  await expect(box(page)).toHaveCount(0);
  await item.locator("[data-name]").click();
  const name = page.getByRole("tree").getByRole("textbox", { name: "New name" });
  await expect(name).toBeFocused();
  await name.fill("New name here");
  await name.press("Enter");
  await expect(page.getByRole("treeitem", { name: "New name here" })).toBeFocused();
  await expect(page.getByRole("heading", { name: "New name here" })).toBeVisible();
});
