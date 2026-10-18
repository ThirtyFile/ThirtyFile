import { generateKeyPairSync } from "node:crypto";
import { expect, test } from "@playwright/test";
import { answer, asAdmin, signIn, signInAsNewUser, unique, USER_PASSWORD } from "./helpers";

function publicKey() {
  const { publicKey } = generateKeyPairSync("ed25519");
  const raw = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  const algorithm = Buffer.from("ssh-ed25519");
  const length = (n: number) => {
    const bytes = Buffer.alloc(4);
    bytes.writeUInt32BE(n);
    return bytes;
  };
  return `ssh-ed25519 ${Buffer.concat([length(algorithm.length), algorithm, length(raw.length), raw]).toString("base64")}`;
}

// The server is shared: SFTP goes off again, so a retry, or a later run on the same server, finds it off as by default
test.afterAll(({ browser }, info) =>
  asAdmin(browser, info, async (request) => expect((await request.put("/api/admin/sftp", { data: { enabled: false, keys_enabled: false } })).ok()).toBe(true)),
);

test("SFTP is disabled by default and administrators can enable access and SSH keys", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/general");
  const access = page.getByRole("switch", { name: "Allow SFTP access", exact: true });
  const keys = page.getByRole("switch", { name: "Allow SSH keys", exact: true });
  await expect(access).not.toBeChecked();
  await expect(keys).not.toBeChecked();
  await expect(page.getByText("Host key fingerprint", { exact: true })).toBeVisible();
  await expect(page.getByText(/^SHA256:/)).toBeVisible();
  const enabled = answer(page, "PUT", "/api/admin/sftp");
  await access.click();
  expect((await enabled).ok()).toBe(true);
  await expect(access).toBeChecked();
  const keysEnabled = answer(page, "PUT", "/api/admin/sftp");
  await keys.click();
  expect((await keysEnabled).ok()).toBe(true);
  await expect(keys).toBeChecked();
});

test("ordinary users add and remove their SSH keys with identity confirmation on desktop and mobile", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const username = await signInAsNewUser(page, "sftp");
  await page.goto("/files");
  await page.getByRole("button", { name: username, exact: true }).click();
  await page.getByRole("menuitem", { name: "SFTP keys", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "SFTP keys" });
  await expect(dialog.getByText("No SSH keys yet")).toBeVisible();
  const name = `Device ${unique()}`;
  await dialog.getByLabel("Name", { exact: true }).fill(name);
  await dialog.getByLabel("SSH public key", { exact: true }).fill(publicKey());
  await dialog.getByLabel("Your current password").fill("incorrect-password");
  const refused = answer(page, "POST", "/api/auth/sftp-keys");
  await dialog.getByRole("button", { name: "Add SSH key" }).click();
  expect((await refused).status()).toBe(400);
  await dialog.getByLabel("Your current password").fill(`${USER_PASSWORD}-2`);
  const added = answer(page, "POST", "/api/auth/sftp-keys");
  await dialog.getByRole("button", { name: "Add SSH key" }).click();
  expect((await added).ok()).toBe(true);
  await expect(dialog.getByText(name, { exact: true })).toBeVisible();
  await expect(dialog.getByText(/^SHA256:/)).toBeVisible();
  await page.setViewportSize({ width: 375, height: 812 });
  await expect(dialog).toBeVisible();
  expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  const removed = answer(page, "DELETE", /\/api\/auth\/sftp-keys\//);
  await dialog.getByRole("button", { name: `Remove SSH key ${name}` }).click();
  expect((await removed).ok()).toBe(true);
  await expect(dialog.getByText("No SSH keys yet")).toBeVisible();
  expect((await page.request.get("/api/admin/sftp")).status()).toBe(403);
  expect(errors).toEqual([]);
});
