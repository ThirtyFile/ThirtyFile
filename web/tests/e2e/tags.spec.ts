// Coloured tags: put on items from the context menu and the details pane (with the mouse and the keyboard), shown as
// dots named for screen readers, listed from the navigation pane, and renamed, recoloured and deleted there. Each test
// signs in as an account of its own: tags are each person's own.
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, openFolder, signInAsNewUser, uploadFile } from "./helpers";

const row = (page: Page, name: string) => page.locator("[data-node-id]").filter({ hasText: name });
const nav = (page: Page) => page.getByRole("navigation", { name: "File locations" });

test("tags are put on items from the menu and the details pane, show by name, and list what has them", async ({ page }) => {
  await signInAsNewUser(page, "tags");
  const dir = await makeFolder(page, "Tagged");
  await uploadFile(page, dir, "a.txt", "a");
  await uploadFile(page, dir, "b.txt", "b");
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await openFolder(page, dir);

  // A new tag from the context menu goes on the item at once
  await row(page, "a.txt").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Tags" }).click();
  await page.getByRole("menuitem", { name: "New tag…" }).click();
  const dialog = page.getByRole("dialog", { name: "New tag" });
  await dialog.getByLabel("Name").fill("Urgent");
  await dialog.locator("label", { hasText: "Blue" }).click();
  await expect(dialog.getByRole("radio", { name: "Blue" })).toBeChecked();
  const made = answer(page, "POST", "/api/tags");
  const tagged = answer(page, "POST", "/api/nodes/tags");
  await dialog.getByRole("button", { name: "Create" }).click();
  expect((await made).ok()).toBe(true);
  expect((await tagged).ok()).toBe(true);
  await expect(row(page, "a.txt").getByRole("img", { name: "Tags: Urgent" })).toBeVisible();
  await expect(row(page, "b.txt").getByRole("img", { name: /^Tags:/ })).toHaveCount(0);

  // Both selected: the tag is on some of them (a dash); choosing it puts it on both
  await row(page, "a.txt").click();
  await row(page, "b.txt").click({ modifiers: ["Shift"] });
  await row(page, "b.txt").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Tags" }).click();
  const urgent = page.getByRole("menuitemcheckbox", { name: "Urgent" });
  await expect(urgent).toHaveAttribute("aria-checked", "mixed");
  const both = answer(page, "POST", "/api/nodes/tags");
  await urgent.click();
  expect((await both).ok()).toBe(true);
  await expect(row(page, "b.txt").getByRole("img", { name: "Tags: Urgent" })).toBeVisible();
  // Once the menu has closed, the focus is back on the list
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(row(page, "b.txt")).toBeFocused();

  // The details pane names them; Edit tags takes the tag off with the keyboard
  await row(page, "a.txt").click();
  await page.keyboard.press("Alt+Enter");
  const pane = page.getByRole("complementary", { name: "Details pane" });
  await expect(pane.getByRole("list", { name: "Tags" })).toHaveText("Urgent");
  const edit = pane.getByRole("button", { name: "Edit tags" });
  await edit.focus();
  await page.keyboard.press("Enter");
  const item = page.getByRole("menuitemcheckbox", { name: "Urgent" });
  await expect(item).toHaveAttribute("aria-checked", "true");
  await expect(item).toBeFocused();
  const off = answer(page, "POST", "/api/nodes/tags");
  await page.keyboard.press("Enter");
  expect((await off).ok()).toBe(true);
  await expect(row(page, "a.txt").getByRole("img", { name: /^Tags:/ })).toHaveCount(0);
  await expect(pane.getByRole("list", { name: "Tags" })).toHaveCount(0);

  // The navigation pane lists the tag; it opens the items that have it, wherever they are
  await nav(page).getByRole("link", { name: "Urgent" }).click();
  await page.waitForURL(/\/tags\/\d+$/);
  await expect(row(page, "b.txt")).toBeVisible();
  await expect(row(page, "a.txt")).toHaveCount(0);
});

test("a tag is renamed, recoloured and deleted, and comes off its items", async ({ page }) => {
  await signInAsNewUser(page, "tag-edit");
  const dir = await makeFolder(page, "Tag edits");
  const file = await uploadFile(page, dir, "report.txt", "r");
  const tag = await (await page.request.post("/api/tags", { data: { name: "Later", color: "red" } })).json();
  expect((await page.request.post("/api/nodes/tags", { data: { ids: [file], add: [tag.id] } })).ok()).toBe(true);
  await page.goto(`/tags/${tag.id}`);
  await expect(row(page, "report.txt")).toBeVisible();

  // Renamed from the bar above the list; the navigation pane follows
  await page.getByRole("button", { name: "Rename…" }).click();
  const dialog = page.getByRole("dialog", { name: "Edit tag" });
  await dialog.getByLabel("Name").fill("Next week");
  const renamed = answer(page, "PATCH", `/api/tags/${tag.id}`);
  await dialog.getByRole("button", { name: "Save" }).click();
  expect((await renamed).ok()).toBe(true);
  await expect(nav(page).getByRole("link", { name: "Next week" })).toBeVisible();

  // Recoloured from the navigation pane's menu
  await nav(page).getByRole("link", { name: "Next week" }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Color" }).click();
  const recoloured = answer(page, "PATCH", `/api/tags/${tag.id}`);
  await page.getByRole("menuitemradio", { name: "Green" }).click();
  expect((await recoloured).ok()).toBe(true);
  await expect(page.getByRole("combobox", { name: "Color" })).toHaveValue("green");

  // Deleted: it comes off the file, and the page goes back to the files
  await page.getByRole("button", { name: "Delete tag" }).click();
  const deleted = answer(page, "DELETE", `/api/tags/${tag.id}`);
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  expect((await deleted).ok()).toBe(true);
  await page.waitForURL(/\/files/);
  await expect(nav(page).getByRole("link", { name: "Next week" })).toHaveCount(0);
  const listed = await (await page.request.get(`/api/nodes/${dir}/children`)).json();
  expect(listed[0].tags).toBeUndefined();
});
