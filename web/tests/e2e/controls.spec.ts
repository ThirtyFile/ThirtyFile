// Tabs, drop-down lists, choices and error messages work with the keyboard and a screen reader.
import { expect, test } from "@playwright/test";
import { signIn } from "./helpers";

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
  const me = await (await page.request.get("/api/auth/me")).json();
  const name = `Expiry ${Date.now().toString(36)}`;
  await page.request.post("/api/folders", { data: { parent_id: me.root_id, name } });
  await page.goto("/files");
  await page.locator("[data-node-id]").filter({ hasText: name }).click({ button: "right" });
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
  const me = await (await page.request.get("/api/auth/me")).json();
  const folder = await (await page.request.post("/api/folders", { data: { parent_id: me.root_id, name: `Locked ${Date.now().toString(36)}` } })).json();
  const share = await (await page.request.post("/api/shares", { data: { node_id: folder.id, password: "right-password-1" } })).json();
  await page.context().clearCookies();
  await page.goto(`/share/${share.id}`);
  const field = page.getByLabel("Password");
  await field.fill("wrong");
  await field.press("Enter");
  await expect(field).toHaveAttribute("aria-invalid", "true");
  const described = await field.getAttribute("aria-describedby");
  await expect(page.locator(`[id="${described}"]`)).toHaveRole("alert");
});
