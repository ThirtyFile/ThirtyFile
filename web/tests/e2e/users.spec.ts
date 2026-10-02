// Searching the users list, and pages that load only what they need.
import { expect, test } from "@playwright/test";
import { makeUser, signIn, unique } from "./helpers";

test("the users list finds accounts on the server by name", async ({ page }) => {
  await signIn(page);
  // The display name is this test's own too: another copy of it may run at the same time
  const tag = unique();
  const zoe = await makeUser(page, "zoe", { display_name: `Zoe Example ${tag}` });
  const yan = await makeUser(page, "yan");
  await page.goto("/admin/users");
  const rows = page.locator("tbody tr");
  await expect(rows.filter({ hasText: yan })).toBeVisible();

  const box = page.getByRole("textbox", { name: "Search users" });
  await box.fill(`zoe example ${tag}`);
  await expect(rows.filter({ hasText: yan })).toHaveCount(0);
  await expect(rows.filter({ hasText: zoe })).toBeVisible();
  await expect(page.getByText("1 user found")).toBeVisible();

  await box.fill(`nobody-${tag}`);
  await expect(page.getByText(`No users match "nobody-${tag}"`)).toBeVisible();
});

test("an account's permissions have the same names in the list and the dialog, and Share says what it covers", async ({ page }) => {
  await signIn(page);
  const name = await makeUser(page, "pat", { can_delete: false });
  await page.goto("/admin/users");
  const row = page.locator("tbody tr").filter({ hasText: name });
  await expect(row).toContainText("Edit, Share");

  await row.dblclick();
  const dialog = page.getByRole("dialog", { name: `Edit "${name}"` });
  await expect(dialog.getByRole("checkbox", { name: "Edit", exact: true })).toBeChecked();
  await expect(dialog.getByRole("checkbox", { name: "Delete", exact: true })).not.toBeChecked();
  await expect(dialog.getByRole("checkbox", { name: "Share", exact: true })).toBeChecked();
  await expect(dialog.getByText("Edit includes uploading. Share includes share links, Share with… and managing the members of a space.")).toBeVisible();
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
