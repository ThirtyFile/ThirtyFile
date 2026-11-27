// An administrator adds a file webhook, pauses it and removes it.
import { expect, test } from "@playwright/test";
import { answer, signIn, unique } from "./helpers";

test("a file webhook is added, paused and removed in the Control panel", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/webhooks");
  const url = `https://hooks.example.com/${unique()}`;
  await page.getByRole("button", { name: "Add webhook" }).click();
  const dialog = page.getByRole("dialog", { name: "Add webhook" });
  const save = dialog.getByRole("button", { name: "Save" });
  await dialog.getByLabel("Webhook address").fill(url);
  await expect(save).toBeDisabled();
  await dialog.getByRole("button", { name: "Generate" }).click();
  // One event few changes make, so the other tests' changes queue little for it
  for (const box of await dialog.getByRole("group", { name: "Events" }).getByRole("checkbox").all()) await box.click();
  await expect(save).toBeDisabled();
  await dialog.getByRole("checkbox", { name: "Delete share link" }).click();
  const added = answer(page, "POST", "/api/admin/file-webhooks");
  await save.click();
  expect((await added).ok()).toBe(true);
  await expect(dialog).toBeHidden();

  const card = page.locator("section").filter({ hasText: url });
  await expect(card).toContainText("Delete share link");
  const paused = answer(page, "PUT", /^\/api\/admin\/file-webhooks\/[^/]+$/);
  await card.getByRole("button", { name: "Pause" }).click();
  expect((await paused).ok()).toBe(true);
  await expect(card.getByRole("button", { name: "Resume" })).toBeVisible();
  await card.getByRole("button", { name: "Remove" }).click();
  const removed = answer(page, "DELETE", /^\/api\/admin\/file-webhooks\/[^/]+$/);
  await page.getByRole("alertdialog").or(page.getByRole("dialog")).getByRole("button", { name: "Remove" }).click();
  expect((await removed).ok()).toBe(true);
  await expect(card).toHaveCount(0);
});
