// Database backups (Control panel › Backups): set up with a passphrase typed twice, then a copy made at once
import { expect, test } from "@playwright/test";
import { answer, signIn } from "./helpers";

test("database backups are set up with a passphrase typed twice, and Back up now makes a copy", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/backups");
  await page.getByRole("button", { name: "Database backups…" }).click();
  const dialog = page.getByRole("dialog", { name: "Database backups" });
  await dialog.getByLabel("Keep the copies on").selectOption({ index: 1 });
  const passphrase = dialog.getByLabel("Passphrase", { exact: true });
  await expect(passphrase).toHaveAttribute("type", "password");
  await passphrase.fill("an end-to-end passphrase");
  await dialog.getByLabel("Passphrase again").fill("not the same passphrase");
  await expect(dialog.getByText("The two passphrases aren't the same")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await dialog.getByLabel("Passphrase again").fill("an end-to-end passphrase");
  const saved = answer(page, "PUT", "/api/admin/backups/database");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  expect((await saved).ok()).toBe(true);
  await expect(dialog).toBeHidden();

  // Opened again: the passphrase is kept and not shown, and a copy is made now
  await page.getByRole("button", { name: "Database backups…" }).click();
  await expect(dialog.getByLabel("New passphrase (leave blank to keep the current one)")).toHaveValue("");
  const run = answer(page, "POST", "/api/admin/backups/database/run");
  await dialog.getByRole("button", { name: "Back up now", exact: true }).click();
  expect((await run).ok()).toBe(true);
  await expect(dialog.getByText(/^Last copy: /)).toBeVisible({ timeout: 30_000 });
  await expect(dialog.getByText(/1 copy on the location/)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByText(/Database: copied /)).toBeVisible();
});
