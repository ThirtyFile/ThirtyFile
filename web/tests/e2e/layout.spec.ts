// The file explorer's layout on a phone and on a desktop, and the arrows of the navigation pane.
import { expect, test, type Page } from "@playwright/test";
import { signIn } from "./helpers";

/** A folder in My files (unique name) holding a folder with a folder in it, a folder without, and a few files */
async function sample(page: Page) {
  const me = await (await page.request.get("/api/auth/me")).json();
  const make = async (parent: string, name: string) => (await (await page.request.post("/api/folders", { data: { parent_id: parent, name } })).json()).id as string;
  const top = await make(me.root_id, `Layout ${Date.now().toString(36)}`);
  const withChild = await make(top, "With folder");
  await make(withChild, "Inside");
  const without = await make(top, "Without folder");
  for (let i = 0; i < 5; i++) await make(top, `Folder ${i}`);
  return { top, without };
}

test("on a phone the toolbar is one row, the path has no scroll bar and large icons come three to a row", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await signIn(page);
  const { top } = await sample(page);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("grid")));
  await page.goto(`/files/${top}`);
  await expect(page.locator("[data-node-id]").first()).toBeVisible();

  // What needs a selection is in the bar that shows below the list: not in the toolbar
  await expect(page.getByRole("button", { name: "Cut", exact: true })).toBeHidden();
  await expect(page.getByRole("button", { name: "Delete", exact: true })).toBeHidden();
  const newButton = page.getByRole("button", { name: "New", exact: true });
  const details = page.getByRole("button", { name: "Details pane" });
  expect((await newButton.boundingBox())!.y).toBe((await details.boundingBox())!.y);
  await expect(newButton).toContainText("New");

  // The path scrolls sideways without a scroll bar of its own
  const trail = page.getByRole("navigation", { name: "File path" });
  expect(await trail.evaluate((el) => (el as HTMLElement).offsetHeight - el.clientHeight)).toBe(0);

  const tops = await page.locator("[data-node-id]").evaluateAll((els) => els.map((el) => Math.round(el.getBoundingClientRect().top)));
  expect(tops.filter((y) => y === tops[0])).toHaveLength(3);
});

test("the file list doesn't scroll sideways at 1280 pixels", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signIn(page);
  const { top } = await sample(page);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${top}`);
  await expect(page.locator("[data-node-id]").first()).toBeVisible();
  const scroller = page.locator("table[role=grid]").locator("xpath=ancestor::*[contains(@class,'overflow-auto')][1]");
  expect(await scroller.evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(0);
});

test("only folders with folders in them have an arrow in the navigation pane", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signIn(page);
  // Inside one of them, the tree shows the folder above it expanded
  const { without: inside } = await sample(page);
  await page.goto(`/files/${inside}`);
  const tree = page.getByRole("tree");
  await expect(tree.getByRole("treeitem", { name: "With folder" })).toHaveAttribute("aria-expanded", "false");
  // No arrow, and not announced as something to expand
  const without = tree.getByRole("treeitem", { name: "Without folder" });
  await expect(without).toBeVisible();
  expect(await without.getAttribute("aria-expanded")).toBeNull();
});
