// Control panel › Administrator alerts: settings are checked and saved, and a webhook on this computer is refused
// until allowed
import { expect, test } from "@playwright/test";
import { answer, asAdmin, signIn } from "./helpers";

test("administrator alerts are saved with a secret, tested against a webhook, and refused on the private network", async ({ page, browser }, info) => {
  await signIn(page);
  await page.goto("/admin/alerts");
  await expect(page.getByRole("heading", { name: "Where alerts go" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Latest messages" })).toBeVisible();

  // A webhook needs a secret: typed, and not shown
  await page.getByLabel("Webhook address (optional)").fill("http://127.0.0.1:9/alerts");
  const secret = page.getByLabel("Shared secret", { exact: true });
  await expect(secret).toHaveAttribute("type", "password");
  await secret.fill("too short");
  await expect(page.getByText("The shared secret needs at least 16 characters")).toBeVisible();
  await expect(page.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await secret.fill("an end-to-end secret of 30 chars");
  const saved = answer(page, "PUT", "/api/admin/alerts");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  expect((await saved).ok()).toBe(true);
  await expect(secret).toHaveValue("");
  await expect(secret).toHaveAttribute("placeholder", "Leave blank to keep the current secret");

  // On this computer: refused until allowed
  const refused = answer(page, "POST", "/api/admin/alerts/test");
  await page.getByRole("button", { name: "Send a test to the webhook" }).click();
  const response = await refused;
  expect(response.status()).toBe(400);
  expect(await response.text()).toContain("private network");
  await expect(page.getByText(/private network/).first()).toBeVisible();

  // Put back as it was, so other tests are not affected
  await asAdmin(browser, info, async (request) => {
    const reset = await request.put("/api/admin/alerts", {
      data: {
        email: true,
        webhook_url: "",
        allow_private: false,
        usage: true,
        offline: true,
        data_disk: true,
        sign_ins: true,
        backups: true,
        certificate: true,
        sign_in_failures: 10,
        sign_in_minutes: 10,
        cooldown_minutes: 30,
      },
    });
    expect(reset.ok()).toBe(true);
  });
});
