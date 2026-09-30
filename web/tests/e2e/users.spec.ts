// Searching the users list, and pages that load only what they need.
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

test("the users list finds accounts on the server by name", async ({ page }) => {
  await signIn(page);
  const suffix = Date.now().toString(36);
  for (const name of [`zoe-${suffix}`, `yan-${suffix}`]) {
    const res = await page.request.post("/api/admin/users", { data: { username: name, password: "a-long-test-password-1", display_name: name === `zoe-${suffix}` ? "Zoe Example" : "" } });
    expect(res.ok()).toBe(true);
  }
  await page.goto("/admin/users");
  const rows = page.locator("tbody tr");
  await expect(rows.filter({ hasText: `yan-${suffix}` })).toBeVisible();

  const box = page.getByRole("textbox", { name: "Search users" });
  await box.fill("zoe example");
  await expect(rows.filter({ hasText: `yan-${suffix}` })).toHaveCount(0);
  await expect(rows.filter({ hasText: `zoe-${suffix}` })).toBeVisible();
  await expect(page.getByText("1 user found")).toBeVisible();

  await box.fill(`nobody-${suffix}`);
  await expect(page.getByText(`No users match "nobody-${suffix}"`)).toBeVisible();
});

test("the sign-in page doesn't load the file explorer", async ({ page }) => {
  const scripts: string[] = [];
  page.on("request", (r) => {
    if (r.resourceType() === "script") scripts.push(new URL(r.url()).pathname);
  });
  await page.goto("/login");
  await expect(page.getByRole("button", { name: "Click or press any key to sign in" })).toBeVisible();
  expect(scripts.filter((s) => /\/(AppShell|FilesPage|FileList|Explorer|ThisPcPage)-/.test(s))).toEqual([]);
});
