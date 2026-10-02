// Lists and settings pages whose data can't be loaded say so, with a way to try again, rather than showing an empty
// list or loading for ever.
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn } from "./helpers";

/** Answers requests to `path` with an error until the returned function is called */
async function failing(page: Page, path: string) {
  const url = `**/api${path}`;
  // A 4xx answer isn't retried, so the error shows at once
  await page.route(url, (route) => route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: "Invalid request" }) }));
  return () => page.unroute(url);
}

test("a list that can't be loaded shows the error and loads again with Try again", async ({ page }) => {
  await signIn(page);
  const recover = await failing(page, "/admin/groups");
  await page.goto("/admin/groups");
  const alert = page.getByRole("alert");
  await expect(alert).toBeVisible();
  await expect(page.getByText("No groups yet")).toHaveCount(0);
  await recover();
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByText("No groups yet")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("a settings page that can't be loaded stops loading and offers Try again", async ({ page }) => {
  await signIn(page);
  const recover = await failing(page, "/admin/settings");
  await page.goto("/admin/usage");
  const alert = page.getByRole("alert");
  await expect(alert.getByRole("button", { name: "Try again" })).toBeVisible();
  await recover();
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("heading", { name: "Storage usage", level: 2 })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("a shared folder whose listing fails doesn't say it is empty", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Shared");
  const share = await (await page.request.post("/api/shares", { data: { node_id: folder } })).json();
  const recover = await failing(page, `/public/shares/${share.id}/nodes/*/children*`);
  await page.goto(`/share/${share.id}`);
  const alert = page.getByRole("alert");
  await expect(alert).toBeVisible();
  await expect(page.getByText("This folder is empty")).toHaveCount(0);
  await recover();
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByText("This folder is empty")).toBeVisible();
});

test("a folder or file that isn't there says so and offers All spaces instead of Try again", async ({ page }) => {
  await signIn(page);
  await page.goto("/files/no-such-folder");
  const alert = page.getByRole("alert");
  await expect(alert.getByText("This folder can't be found")).toBeVisible();
  await expect(alert.getByRole("button", { name: "Try again" })).toHaveCount(0);
  await alert.getByRole("link", { name: "Go to All spaces" }).click();
  await expect(page).toHaveURL(/\/drives$/);

  await page.goto("/view/no-such-file");
  await expect(page.getByRole("alert").getByText("This file can't be found")).toBeVisible();
  await expect(page.getByRole("alert").getByRole("link", { name: "Go to All spaces" })).toBeVisible();
});

test("a server that can't be reached is said so in words, not with the browser's Failed to fetch", async ({ page }) => {
  await signIn(page);
  await page.route("**/api/admin/groups*", (route) => route.abort("internetdisconnected"));
  await page.goto("/admin/groups");
  const alert = page.getByRole("alert");
  // Tried three times first: the server may be back a moment later
  await expect(alert.getByText("Can't reach the server. Check your connection and try again.")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText("Failed to fetch")).toHaveCount(0);
  await page.unroute("**/api/admin/groups*");
  await alert.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByText("No groups yet")).toBeVisible();
});
