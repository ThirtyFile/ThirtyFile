// The Windows style's context menus, as in Windows 11: View, Sort by, Group by, Undo, New and Upload on empty space,
// a row of icon buttons on items, and both opened and gone through from the keyboard
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

const row = (page: Page, name: string) => page.locator("[data-node-id]").filter({ hasText: name });

/** A folder with a few items, open in the Details view */
async function setUp(page: Page, name: string) {
  await signIn(page);
  const dir = await makeFolder(page, name);
  await makeFolder(page, "Photos", dir);
  await uploadFile(page, dir, "notes.txt", "a few notes");
  await uploadFile(page, dir, "big.txt", "x".repeat(5000));
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${dir}`);
  await expect(row(page, "notes.txt")).toBeVisible();
  return dir;
}

/** Right-clicks the empty space below the items */
async function emptySpaceMenu(page: Page) {
  const area = page.locator("[data-explorer-area]");
  const box = (await area.boundingBox())!;
  await area.click({ button: "right", position: { x: 40, y: box.height - 30 } });
  await expect(page.getByRole("menu")).toBeVisible();
}

const menu = (page: Page) => page.getByRole("menu").first();

/** Chooses an item of the open menus, and waits for them to go */
async function choose(page: Page, role: "menuitem" | "menuitemradio", name: string) {
  await page.getByRole(role, { name, exact: true }).click();
  await expect(page.getByRole("menu")).toHaveCount(0);
}

/** The accessible name of what has the focus */
const focused = (page: Page) => page.evaluate(() => document.activeElement?.getAttribute("aria-label") ?? document.activeElement?.textContent?.trim());

test("the empty space's menu is in Windows 11's order, and View, Sort by and Group by share the toolbar's choices", async ({ page }) => {
  await setUp(page, "Menus");
  await row(page, "notes.txt").click();
  await emptySpaceMenu(page);
  // Right-clicking empty space leaves nothing selected
  await expect(page.locator("[data-node-id][aria-selected=true]")).toHaveCount(0);
  const names = await menu(page)
    .locator(":scope > [role=menuitem]")
    .evaluateAll((items) => items.map((el) => el.textContent?.replace(/Ctrl.*|F5/, "").trim()));
  // Nothing to undo yet
  expect(names).toEqual(["View", "Sort by", "Group by", "Refresh", "Paste", "New", "Upload", "Properties"]);

  // View: the toolbar's view is checked, and choosing another changes it
  await menu(page).getByRole("menuitem", { name: "View" }).click();
  await expect(page.getByRole("menuitemradio", { name: "Details" })).toHaveAttribute("aria-checked", "true");
  await choose(page, "menuitemradio", "Large icons");
  await expect(page.getByRole("listbox", { name: "Items" })).toBeVisible();
  await page.getByRole("button", { name: "View" }).click();
  await expect(page.getByRole("menuitemradio", { name: "Large icons" })).toHaveAttribute("aria-checked", "true");
  await choose(page, "menuitemradio", "Details");
  await expect(page.getByRole("grid", { name: "Items" })).toBeVisible();

  // Sort by: the list and the toolbar follow
  await emptySpaceMenu(page);
  await menu(page).getByRole("menuitem", { name: "Sort by" }).click();
  await expect(page.getByRole("menuitemradio", { name: "Name" })).toHaveAttribute("aria-checked", "true");
  await choose(page, "menuitemradio", "Size");
  await expect(page.getByRole("columnheader", { name: /^Size/ })).toHaveAttribute("aria-sort", "ascending");
  await emptySpaceMenu(page);
  await menu(page).getByRole("menuitem", { name: "Sort by" }).click();
  await choose(page, "menuitemradio", "Descending");
  await expect(page.getByRole("columnheader", { name: /^Size/ })).toHaveAttribute("aria-sort", "descending");
  await page.getByRole("button", { name: "Sort" }).click();
  await expect(page.getByRole("menuitemradio", { name: "Size" })).toHaveAttribute("aria-checked", "true");
  await expect(page.getByRole("menuitemradio", { name: "Descending" })).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);

  // Group by: headings show, and the toolbar's View menu has the same choice
  await emptySpaceMenu(page);
  await menu(page).getByRole("menuitem", { name: "Group by" }).click();
  await expect(page.getByRole("menuitemradio", { name: "(None)" })).toHaveAttribute("aria-checked", "true");
  await choose(page, "menuitemradio", "Type");
  await expect(page.getByRole("gridcell", { name: "File folder (1)" })).toBeVisible();
  await page.getByRole("button", { name: "View" }).click();
  await page.getByRole("menuitem", { name: "Group by" }).click();
  await expect(page.getByRole("menuitemradio", { name: "Type" })).toHaveAttribute("aria-checked", "true");
});

test("New › Folder in the empty space's menu makes a folder and starts renaming it", async ({ page }) => {
  const dir = await setUp(page, "New from menu");
  await emptySpaceMenu(page);
  await menu(page).getByRole("menuitem", { name: "New" }).click();
  await choose(page, "menuitem", "Folder Ctrl+Shift+N");
  const box = page.getByRole("textbox", { name: "New name" });
  await expect(box).toBeFocused();
  await expect(box).toHaveValue("New folder");
  await box.fill("Projects");
  await box.press("Enter");
  await expect(row(page, "Projects")).toBeVisible();
  const items: { name: string }[] = await (await page.request.get(`/api/nodes/${dir}/children`)).json();
  expect(items.map((n) => n.name)).toContain("Projects");
});

test("Undo in the empty space's menu is named for what it takes back, and takes back a delete", async ({ page }) => {
  await setUp(page, "Undo from menu");
  await row(page, "notes.txt").click();
  await page.keyboard.press("Delete");
  await page.getByRole("dialog").getByRole("button", { name: "Move to trash" }).click();
  await expect(row(page, "notes.txt")).toHaveCount(0);

  await emptySpaceMenu(page);
  const undo = menu(page).getByRole("menuitem", { name: /^Undo delete/ });
  await expect(undo).toBeVisible();
  await undo.click();
  await expect(row(page, "notes.txt")).toBeVisible();
  // Taken back: nothing left to undo
  await emptySpaceMenu(page);
  await expect(menu(page).getByRole("menuitem", { name: /^Undo/ })).toHaveCount(0);
});

test("an item's menu has a row of icon buttons with names and tooltips, disabled where they can't be used", async ({ page }) => {
  await setUp(page, "Icon row");
  await row(page, "notes.txt").click({ button: "right" });
  const icons = menu(page).getByRole("group", { name: "Common actions" }).getByRole("menuitem");
  await expect(icons).toHaveCount(5);
  const named = await icons.evaluateAll((items) => items.map((el) => [el.getAttribute("aria-label"), el.getAttribute("title")]));
  expect(named).toEqual([
    ["Cut", "Cut (Ctrl+X)"],
    ["Copy", "Copy (Ctrl+C)"],
    ["Rename", "Rename (F2)"],
    ["Share", "Share"],
    ["Delete", "Delete (Delete)"],
  ]);
  // Below the row, the other commands with their shortcuts on the right
  await expect(menu(page).getByRole("menuitem", { name: "Properties Alt+Enter" })).toBeVisible();
  await choose(page, "menuitem", "Rename");
  await expect(page.getByRole("textbox", { name: "New name" })).toHaveValue("notes.txt");
  await page.keyboard.press("Escape");

  // Two items: Rename and Share work on one item only
  await row(page, "notes.txt").click();
  await row(page, "big.txt").click({ modifiers: ["Control"] });
  await row(page, "big.txt").click({ button: "right" });
  await expect(menu(page).getByRole("menuitem", { name: "Rename", exact: true })).toHaveAttribute("aria-disabled", "true");
  await expect(menu(page).getByRole("menuitem", { name: "Share", exact: true })).toHaveAttribute("aria-disabled", "true");
  await expect(menu(page).getByRole("menuitem", { name: "Copy", exact: true })).not.toHaveAttribute("aria-disabled", "true");
  await choose(page, "menuitem", "Copy");
  await expect(page.getByText("2 items copied", { exact: true })).toBeVisible();
});

test("Shift+F10 and the Menu key open the menus, and the arrow keys go through the icon row and into submenus", async ({ page }) => {
  await setUp(page, "Menu keys");

  // An item's menu from the keyboard: Down reaches the icon row, Left and Right go along it, Down leaves it
  await row(page, "notes.txt").click();
  await page.keyboard.press("Shift+F10");
  await expect(menu(page)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe("Cut");
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  expect(await focused(page)).toBe("Rename");
  await page.keyboard.press("ArrowLeft");
  expect(await focused(page)).toBe("Copy");
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe("Open");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  // The focus is back on the item, and the Menu key opens its menu too
  await expect(row(page, "notes.txt")).toBeFocused();
  await page.keyboard.press("ContextMenu");
  await expect(menu(page)).toBeFocused();
  await expect(menu(page).getByRole("menuitem", { name: "Cut", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);

  // Nothing selected, the focus outside the items: the empty space's menu, and Right goes into a submenu
  await page.keyboard.press("Escape");
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.press("Shift+F10");
  await expect(menu(page)).toBeFocused();
  await expect(menu(page).getByRole("menuitem", { name: "View" })).toBeVisible();
  await page.keyboard.press("ArrowDown");
  expect(await focused(page)).toBe("View");
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("menuitemradio", { name: "Large icons" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("listbox", { name: "Items" })).toBeVisible();
});
