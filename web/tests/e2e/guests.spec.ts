// Guest conversion requires a current preview, explicit approval, and administrator identity confirmation.
import { expect, test } from "@playwright/test";
import { PASSWORD, USER_PASSWORD, makeFolder, makeUser, signIn, unique } from "./helpers";

for (const [label, viewport] of [
  ["desktop", { width: 1280, height: 900 }],
  ["mobile", { width: 390, height: 844 }],
] as const) {
  test(`guest conversion is reviewed and reversible on ${label}`, async ({ page }) => {
    await page.setViewportSize(viewport);
    await signIn(page);
    const name = await makeUser(page, "guest-conversion");
    const users = await (await page.request.get("/api/admin/users")).json();
    const user = users.find((u: { username: string }) => u.username === name);
    const shared = await makeFolder(page, "Client");
    expect((await page.request.post(`/api/nodes/${shared}/access`, { data: { principal_type: "user", principal_id: user.id, role: "manager", expires_at: null } })).ok()).toBe(true);
    await page.goto("/admin/users");
    const row = page.locator("tbody tr").filter({ hasText: name });
    await row.dblclick();
    const dialog = page.getByRole("dialog", { name: `Edit "${name}"` });
    await dialog.getByRole("button", { name: "Guest", exact: true }).click();
    await expect(dialog.getByText("Guest conversion preview", { exact: true })).toBeVisible();
    await expect(dialog.getByText(/These direct grants change to Editor or below/)).toBeVisible();
    const save = dialog.getByRole("button", { name: "Save", exact: true });
    await expect(save).toBeDisabled();
    await dialog.getByLabel("Your current password").fill(PASSWORD);
    await expect(save).toBeDisabled();
    await dialog.getByRole("checkbox", { name: "I have reviewed and agree to these guest account changes" }).check();
    await expect(save).toBeEnabled();
    await save.click();
    await expect(dialog).toBeHidden({ timeout: 20_000 });
    await expect(row).toContainText("Guest");
    const access = await (await page.request.get(`/api/nodes/${shared}/access`)).json();
    const grant = access.grants.find((g: { principal_id: number }) => g.principal_id === user.id);
    expect(grant.role).toBe("editor");
    expect(grant.expires_at).toBeGreaterThan(Math.floor(Date.now() / 1000));
    await row.dblclick();
    await dialog.getByRole("button", { name: "Standard user", exact: true }).click();
    await dialog.getByLabel("Your current password").fill(PASSWORD);
    await save.click();
    await expect(dialog).toBeHidden({ timeout: 20_000 });
    const current = (await (await page.request.get("/api/admin/users")).json()).find((u: { id: number }) => u.id === user.id);
    expect(current.guest).toBe(false);
    expect(current.personal_space).toBe(true);
  });
}

test("new guests have no personal space and the account list shows their type", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/users");
  await page.getByRole("button", { name: "Add user", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Add user" });
  const name = `guest-${unique()}`;
  await dialog.getByLabel("Username", { exact: true }).fill(name);
  await dialog.getByLabel(/Password \(at least/).fill(USER_PASSWORD);
  await dialog.getByRole("button", { name: "Guest", exact: true }).click();
  await expect(dialog.getByRole("checkbox", { name: 'Create "My files" (a private space only they can see)' })).toBeDisabled();
  await dialog.getByRole("button", { name: "Create", exact: true }).click();
  await expect(dialog).toBeHidden({ timeout: 20_000 });
  const row = page.locator("tbody tr").filter({ hasText: name });
  await expect(row).toContainText("Guest");
  await expect(row).toContainText('No "My files"');
  const current = (await (await page.request.get("/api/admin/users")).json()).find((u: { username: string }) => u.username === name);
  expect(current.guest).toBe(true);
  expect(current.personal_space).toBe(false);
});
