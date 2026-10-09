// Tabs, drop-down lists, choices and error messages work with the keyboard and a screen reader.
import { expect, test } from "@playwright/test";
import { answer, makeFolder, makeUser, openFolder, signIn, signInAsNewUser, uploadFile } from "./helpers";

test("Tab doesn't stop on the tabs' close buttons, and Delete closes the focused tab", async ({ page }) => {
  await signIn(page);
  for (let i = 0; i < 3; i++) await page.getByRole("button", { name: "New tab" }).click();
  const tabs = page.getByRole("tablist", { name: "Tabs" }).getByRole("tab");
  await expect(tabs).toHaveCount(4);
  // Only the active tab is a Tab stop, and after it comes the New tab button: no close buttons in between
  const active = page.getByRole("tab", { selected: true });
  await active.focus();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "New tab" })).toBeFocused();
  await active.focus();
  await page.keyboard.press("Delete");
  await expect(tabs).toHaveCount(3);
  await expect(page.getByRole("tab", { selected: true })).toBeFocused();
});

test("drop-down lists have the same border as the text fields", async ({ page }) => {
  await signIn(page);
  await page.goto("/shares");
  const select = page.getByRole("combobox", { name: "Show" });
  const search = page.getByRole("textbox").first();
  const border = (el: HTMLElement | SVGElement) => getComputedStyle(el).borderTopColor;
  expect(await select.evaluate(border)).toBe(await search.evaluate(border));
});

test("the log tabs move with the arrow keys and are tied to their log", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/activity");
  const list = page.getByRole("tablist", { name: "Activity log" });
  const first = list.getByRole("tab").first();
  await first.click();
  await expect(first).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowRight");
  const second = list.getByRole("tab").nth(1);
  await expect(second).toBeFocused();
  await expect(second).toHaveAttribute("aria-selected", "true");
  const panel = await second.getAttribute("aria-controls");
  expect(panel).toBeTruthy();
  await expect(page.locator(`[id="${panel}"]`)).toHaveAttribute("role", "tabpanel");
});

test("a share link's expiry choices say which is chosen", async ({ page }) => {
  await signIn(page);
  // In a folder of its own: My files holds the other tests' folders too
  const dir = await makeFolder(page, "Expiry");
  await makeFolder(page, "Shared", dir);
  await page.goto(`/files/${dir}`);
  await page.locator("[data-node-id]").filter({ hasText: "Shared" }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Create share link" }).click();
  const group = page.getByRole("group", { name: "Expiration" });
  await expect(group.getByRole("button", { pressed: true })).toHaveCount(1);
  const label = await group.getByRole("button", { pressed: false }).first().textContent();
  const other = group.getByRole("button", { name: label!, exact: true });
  await other.click();
  await expect(other).toHaveAttribute("aria-pressed", "true");
  await expect(group.getByRole("button", { pressed: true })).toHaveCount(1);
});

test("a wrong share password marks the field and points it to the message", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Locked");
  const share = await (await page.request.post("/api/shares", { data: { node_id: folder, password: "right-password-1" } })).json();
  await page.context().clearCookies();
  await page.goto(`/share/${share.id}`);
  const field = page.getByLabel("Password");
  await field.fill("wrong");
  await field.press("Enter");
  await expect(field).toHaveAttribute("aria-invalid", "true");
  const described = await field.getAttribute("aria-describedby");
  await expect(page.locator(`[id="${described}"]`)).toHaveRole("alert");
});

/** Where the keyboard focus is: inside a dialog, or nowhere in particular (the page's body) */
const focusInDialog = (page: import("@playwright/test").Page) => page.evaluate(() => !!document.activeElement?.closest("[role=dialog]"));
const focusLost = (page: import("@playwright/test").Page) => page.evaluate(() => !document.activeElement || document.activeElement === document.body);

test("Tab and Shift+Tab stay inside an open dialog", async ({ page }) => {
  await signIn(page);
  // The account button is read as the name it shows, not with its initial in front ("aadmin")
  await expect(page.getByRole("button", { name: "admin", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "New tag" }).click();
  const dialog = page.getByRole("dialog", { name: "New tag" });
  await expect(dialog.getByLabel("Name")).toBeFocused();
  for (const key of ["Tab", "Tab", "Tab", "Tab", "Tab", "Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab", "Shift+Tab"]) {
    await page.keyboard.press(key);
    // Past the last control the focus meets a guard just outside the dialog, which sends it back to the first: settled,
    // it is inside again
    await expect.poll(() => focusInDialog(page), { message: `after ${key}` }).toBe(true);
  }
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
});

test("after a wrong password is acknowledged, the focus is back in the password field", async ({ page, browser, baseURL }) => {
  await signIn(page);
  // An account of its own: wrong passwords slow down further sign-ins to the account, which the other tests use
  const username = await makeUser(page, "wrong-password");
  const fresh = await browser.newContext({ baseURL });
  const login = await fresh.newPage();
  await login.goto("/login");
  await login.getByRole("button", { name: "Click or press any key to sign in" }).click();
  await expect(login.getByLabel("Username")).toBeFocused();
  await login.getByLabel("Username").fill(username);
  await login.getByLabel("Password").fill("not the password");
  await login.getByRole("button", { name: "Sign in", exact: true }).click();
  await login.getByRole("button", { name: "OK" }).click();
  await expect(login.getByLabel("Password")).toBeFocused();
  await fresh.close();
});

test("after confirming Move to trash, the focus is in the list, not lost", async ({ page }) => {
  await signInAsNewUser(page, "trash-focus");
  const dir = await makeFolder(page, "Trash focus");
  await uploadFile(page, dir, "a.txt", "a");
  await uploadFile(page, dir, "b.txt", "b");
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await openFolder(page, dir);
  await page.locator("[data-node-id]").filter({ hasText: "a.txt" }).click();
  await page.keyboard.press("Delete");
  const trashed = answer(page, "POST", "/api/nodes/trash");
  await page.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  await expect(page.locator("[data-node-id]").filter({ hasText: "a.txt" })).toHaveCount(0);
  await expect.poll(() => focusLost(page)).toBe(false);
});
