// Control panel › Access review: the views load, a reminder is set, and a review is marked as done
import { expect, test } from "@playwright/test";
import { answer, signIn } from "./helpers";

test("the access review shows its views, asks for reviews and is marked reviewed", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/access");
  await expect(page.getByRole("heading", { name: "Spaces and folders" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "People" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Outside access" })).toBeVisible();
  // The administrator is among the people
  await expect(page.locator("details summary").filter({ hasText: "admin" }).first()).toBeVisible();

  const reminder = answer(page, "PUT", "/api/admin/access-review/reminder");
  await page.getByLabel("Ask for a review").selectOption({ label: "Every 90 days" });
  expect((await reminder).ok()).toBe(true);
  await expect(page.getByText(/Next review due /)).toBeVisible();

  const marked = answer(page, "POST", "/api/admin/access-review/reviewed");
  await page.getByRole("button", { name: "Mark as reviewed" }).click();
  expect((await marked).ok()).toBe(true);
  await expect(page.getByText(/Last reviewed .* by admin/)).toBeVisible();
});
