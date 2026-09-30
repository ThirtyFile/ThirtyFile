// Shared by the end-to-end tests.
import { expect, type Page } from "@playwright/test";

export const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

/**
 * Signs in as the administrator. The sign-in page opens on a lock screen and moves the focus to the username field
 * once it shows: typing waits for that, so a busy machine doesn't send the password into the username field.
 */
export async function signIn(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "Click or press any key to sign in" }).click();
  const username = page.getByLabel("Username");
  await expect(username).toBeFocused();
  await username.fill("admin");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(/\/files/);
}
