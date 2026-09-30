// The smaller lists (control panel, admin tables, storage locations, all spaces) work with the keyboard like the file
// list, and tell screen readers which item is selected.
import { expect, test, type Page } from "@playwright/test";

const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

async function signIn(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "Click or press any key to sign in" }).click();
  await page.getByLabel("Username").fill("admin");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(/\/files/);
}

/** How many of the page's options can be reached with Tab */
const tabStops = (page: Page) => page.getByRole("option").evaluateAll((els) => els.filter((el) => (el as HTMLElement).tabIndex === 0).length);

test("the control panel's tiles are one Tab stop, and the arrows and Enter work", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin");
  const users = page.getByRole("option", { name: /^Users/ });
  await expect(users).toBeVisible();
  // Every category is a list of its own, and together they are one Tab stop
  expect(await page.getByRole("listbox").count()).toBeGreaterThan(1);
  expect(await tabStops(page)).toBe(1);
  await users.click();
  await expect(users).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowRight");
  const next = page.getByRole("option", { selected: true });
  await expect(next).toHaveCount(1);
  await expect(next).not.toHaveAccessibleName(/^Users/);
  await expect(next).toBeFocused();
  await page.keyboard.press("Home");
  await expect(users).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await page.waitForURL("**/admin/users");

  // Details view: a grid of rows, with a focus ring and the arrows
  await page.goto("/admin");
  await page.getByRole("button", { name: "Details" }).click();
  const grid = page.getByRole("grid", { name: "Control panel" });
  await grid.getByRole("row", { name: /^Users/ }).click();
  await page.keyboard.press("ArrowDown");
  await expect(grid.getByRole("row", { selected: true })).toHaveCount(1);
  await expect(grid.getByRole("row", { name: /^Users/ })).toHaveAttribute("aria-selected", "false");
});

test("an admin table is a grid: the arrows select the next row and Enter opens it", async ({ page }) => {
  await signIn(page);
  const suffix = Date.now().toString(36);
  for (const name of [`kim-${suffix}`, `lee-${suffix}`]) {
    expect((await page.request.post("/api/admin/users", { data: { username: name, password: "a-long-test-password-1" } })).ok()).toBe(true);
  }
  await page.goto("/admin/users");
  const grid = page.getByRole("grid", { name: "Users" });
  const kim = grid.getByRole("row").filter({ hasText: `kim-${suffix}` });
  await kim.click();
  await expect(kim).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowDown");
  const lee = grid.getByRole("row").filter({ hasText: `lee-${suffix}` });
  await expect(lee).toHaveAttribute("aria-selected", "true");
  await expect(lee).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toContainText(`lee-${suffix}`);
});

test("storage locations are selected and opened with the keyboard", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/storage");
  const grid = page.getByRole("grid", { name: "Storage locations" });
  const row = grid.getByRole("row").first();
  await expect(row).toHaveAttribute("tabindex", "0");
  await row.focus();
  await page.keyboard.press("Space");
  await expect(row).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toBeVisible();
});

test("all spaces: arrows, Shift+arrows and Ctrl+A select like the file list", async ({ page }) => {
  await signIn(page);
  await page.evaluate(() => localStorage.setItem("tf-drives-view", JSON.stringify("list")));
  await page.goto("/drives");
  const grid = page.getByRole("grid", { name: "All spaces" });
  const rows = grid.getByRole("row").filter({ has: page.getByRole("gridcell") });
  await expect(rows.first()).toBeVisible();
  const count = await rows.count();
  expect(count).toBeGreaterThanOrEqual(2);
  await rows.first().click();
  await page.keyboard.press("Shift+ArrowDown");
  await expect(grid.getByRole("row", { selected: true })).toHaveCount(2);
  await page.keyboard.press("ArrowUp");
  await expect(grid.getByRole("row", { selected: true })).toHaveCount(1);
  await expect(rows.first()).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Control+a");
  await expect(grid.getByRole("row", { selected: true })).toHaveCount(count);
  await page.keyboard.press("Escape");
  await expect(grid.getByRole("row", { selected: true })).toHaveCount(0);
});
