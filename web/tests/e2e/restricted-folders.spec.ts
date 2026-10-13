// Owners confirm inheritance changes; a space manager sees only their explicitly permitted files afterwards.
import { expect, test } from "@playwright/test";
import { USER_PASSWORD, answer, makeFolder, makeUser, openFolder, signIn, unique, uploadFile } from "./helpers";

test("an owner previews restricted access and a manager needs a direct folder grant", async ({ page, browser, baseURL }) => {
  await signIn(page);
  expect((await page.request.put("/api/auth/style", { data: { style: "windows" } })).ok()).toBe(true);
  const space = await page.request.post("/api/drives", { data: { name: `Department ${unique()}` } });
  expect(space.ok()).toBe(true);
  const root = (await space.json()).root_id as string;
  const department = await makeFolder(page, "Department", root);
  const hr = await makeFolder(page, "HR", department);
  await uploadFile(page, department, "minutes.txt", "Public minutes");
  await uploadFile(page, hr, "payroll.txt", "Confidential payroll");
  const username = await makeUser(page, "manager");
  const [member] = (await (await page.request.get(`/api/admin/users?q=${username}`)).json()) as { id: number }[];
  expect((await page.request.post(`/api/nodes/${root}/access`, { data: { principal_type: "user", principal_id: member.id, role: "manager" } })).ok()).toBe(true);
  await openFolder(page, department);
  await page
    .locator("[data-node-id]")
    .filter({ has: page.getByText("HR", { exact: true }) })
    .click();
  await page.getByRole("button", { name: "Share with…", exact: true }).click();
  const access = page.getByRole("dialog", { name: "Share “HR”" });
  await expect(access.getByRole("button", { name: "Restrict folder", exact: true })).toBeEnabled();
  await access.getByRole("button", { name: "Restrict folder", exact: true }).click();
  const confirmation = page.getByRole("dialog", { name: "Restrict “HR”?" });
  await expect(confirmation).toContainText(username);
  await expect(confirmation).toContainText("Direct grants remain");
  await confirmation.getByRole("button", { name: "Cancel", exact: true }).click();
  expect((await (await page.request.get(`/api/nodes/${hr}`)).json()).node.restricted).toBe(false);
  await access.getByRole("button", { name: "Restrict folder", exact: true }).click();
  const changed = answer(page, "POST", `/api/nodes/${hr}/restriction`);
  await confirmation.getByRole("button", { name: "Change", exact: true }).click();
  expect((await changed).ok()).toBe(true);
  await expect(access.getByText("Restricted folder", { exact: true })).toBeVisible();
  await expect(access.getByRole("button", { name: "Restore inherited access", exact: true })).toBeEnabled();

  const memberContext = await browser.newContext({ baseURL, locale: "en-US" });
  const manager = await memberContext.newPage();
  expect((await manager.request.post("/api/auth/login", { data: { username, password: USER_PASSWORD } })).ok()).toBe(true);
  expect((await manager.request.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  await openFolder(manager, department);
  await expect(manager.locator("[data-node-id]").filter({ hasText: "minutes.txt" })).toBeVisible();
  await expect(manager.locator("[data-node-id]").filter({ hasText: "HR" })).toHaveCount(0);
  expect((await manager.request.get(`/api/nodes/${hr}`)).status()).toBe(404);

  expect((await page.request.post(`/api/nodes/${hr}/access`, { data: { principal_type: "user", principal_id: member.id, role: "viewer" } })).ok()).toBe(true);
  await openFolder(manager, hr);
  await expect(manager.locator("[data-node-id]").filter({ hasText: "payroll.txt" })).toBeVisible();
  await manager.locator("[data-node-id]").filter({ hasText: "payroll.txt" }).click();
  await expect(manager.getByRole("button", { name: "Delete", exact: true })).toBeDisabled();
  await memberContext.close();
});
