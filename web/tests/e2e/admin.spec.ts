// Every page of the Control panel opens from its tile, without an error, and only for administrators
import { expect, test } from "@playwright/test";
import { signIn } from "./helpers";

const PAGES = [
  "Users",
  "Groups",
  "All share links",
  "Single sign-on",
  "Spaces",
  "Storage locations",
  "Moves",
  "Backups",
  "Replicas",
  "Storage usage",
  "General",
  "Branding",
  "Email",
  "Activity log",
  "Log settings",
];

test("each Control panel tile opens its page, which loads without an error", async ({ page }) => {
  const failed: string[] = [];
  page.on("response", (r) => {
    if (r.url().includes("/api/") && r.status() >= 500) failed.push(`${r.status()} ${r.url()}`);
  });
  page.on("pageerror", (e) => failed.push(e.message));
  await signIn(page);
  for (const name of PAGES) {
    await page.goto("/admin");
    await page.getByRole("option", { name: new RegExp(`^${name}`) }).dblclick();
    // The address bar says where the page is: Control panel › its name
    await expect(page.getByRole("navigation", { name: "File path" }).getByText(name, { exact: true })).toBeVisible();
    await expect(page.getByText(/Couldn't load|Try again/)).toHaveCount(0);
  }
  expect(failed).toEqual([]);
});

test("old and unknown addresses under /admin go to the Control panel or the files", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/system");
  await page.waitForURL(/\/admin$/);
  await page.goto("/admin/nothing-here");
  await page.waitForURL(/\/files/);
});

test("someone who isn't an administrator is sent to their files", async ({ page }) => {
  await signIn(page);
  const name = `plain-${Date.now().toString(36)}`;
  expect((await page.request.post("/api/admin/users", { data: { username: name, password: "a-long-test-password-1" } })).ok()).toBe(true);
  await page.request.post("/api/auth/logout");
  expect((await page.request.post("/api/auth/login", { data: { username: name, password: "a-long-test-password-1" } })).ok()).toBe(true);
  // A password an administrator chose is replaced first
  expect((await page.request.put("/api/auth/password", { data: { current: "a-long-test-password-1", new: "another-long-password-2" } })).ok()).toBe(true);
  await page.goto("/admin/users");
  await page.waitForURL(/\/files/);
});
