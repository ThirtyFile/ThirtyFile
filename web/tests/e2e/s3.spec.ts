import { test, expect } from "@playwright/test";
import { join } from "node:path";
import { mkdir } from "node:fs/promises";
import { asAdmin, signIn, makeFolder, PASSWORD, unique } from "./helpers";

test.describe.configure({ mode: "serial" });

// The server is shared: S3 goes off again, so a retry, or a later run on the same server, finds it off as by default
test.afterAll(({ browser }, info) =>
  asAdmin(browser, info, async (request) => {
    const off = { enabled: false, hostname: "", region: "us-east-1", managers_publish: false };
    expect((await request.put("/api/admin/s3", { data: off })).ok()).toBe(true);
  }),
);

test("administrators configure S3, publish from a folder menu and manage keys shown only once", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await signIn(page);
  await page.goto("/admin/general");
  await expect(page.getByRole("heading", { name: "Apps and protocols", exact: true })).toBeVisible();
  const form = page.locator("form").filter({ has: page.getByRole("heading", { name: "S3", exact: true }) });
  await expect(form.getByRole("checkbox", { name: "Enable S3 access" })).not.toBeChecked();
  await form.getByLabel("S3 hostname").fill("s3.files.example.com");
  await form.getByRole("checkbox", { name: "Enable S3 access" }).check();
  const saved = page.waitForResponse((res) => res.url().endsWith("/api/admin/s3") && res.request().method() === "PUT");
  await form.getByRole("button", { name: "Save", exact: true }).click();
  expect((await saved).ok()).toBe(true);
  const drives = await (await page.request.get("/api/admin/drives")).json();
  const company = drives.find((drive: { kind: string }) => drive.kind === "company");
  const name = `Published ${unique()}`;
  const node = await makeFolder(page, name, company.root_id);
  await page.goto(`/files/${company.root_id}`);
  const row = page.getByRole("row").filter({ hasText: name });
  await row.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Publish as S3 bucket…", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "S3 buckets", exact: true });
  const bucket = `browser-${unique()}`;
  await dialog.getByLabel("Bucket name").fill(bucket);
  // Making credentials asks who is acting
  await dialog.getByLabel("Your current password").fill(PASSWORD);
  await dialog.getByRole("button", { name: "Publish", exact: true }).click();
  const card = dialog.getByRole("region", { name: bucket });
  await expect(card.getByRole("heading", { name: bucket, exact: true })).toBeVisible();
  await card.getByRole("button", { name: "New S3 key" }).click();
  const keyDialog = page.getByRole("dialog", { name: "New S3 key", exact: true });
  await keyDialog.getByLabel("Key description").fill("Browser contract key");
  await keyDialog.getByLabel("Your current password").fill(PASSWORD);
  await keyDialog.getByRole("button", { name: "Create key" }).click();
  await expect(keyDialog.getByText("Save the Secret Key now. It is shown only once.")).toBeVisible();
  const access = await keyDialog.getByLabel("Access Key", { exact: true }).inputValue();
  const secretField = keyDialog.getByLabel("Secret Key", { exact: true });
  const secret = await secretField.inputValue();
  expect(access).toMatch(/^TF[A-F0-9]+$/);
  expect(secret.length).toBeGreaterThan(30);
  // As dots, and copied without being shown
  await expect(secretField).toHaveAttribute("type", "password");
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await keyDialog.getByRole("button", { name: "Copy credentials", exact: true }).click();
  await expect(page.getByText("Copied", { exact: true })).toBeVisible();
  // Windows gives the clipboard's line break as CRLF
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied.replace(/\r\n/g, "\n")).toBe(`AWS_ACCESS_KEY_ID=${access}\nAWS_SECRET_ACCESS_KEY=${secret}`);
  await expect(secretField).toHaveAttribute("type", "password");
  await keyDialog.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByLabel("Secret Key", { exact: true })).toHaveCount(0);
  const response = await page.request.get(`/api/s3/buckets?node_id=${node}`);
  const info = await response.json();
  expect(JSON.stringify(info)).not.toContain(secret);
  expect(info.buckets[0].keys[0].writable).toBe(false);
  await card.getByRole("button", { name: "Revoke key" }).click();
  await expect(card.getByRole("button", { name: "Enable key", exact: true })).toBeVisible();
  await card.getByRole("button", { name: "Stop bucket", exact: true }).click();
  await expect(card.getByRole("button", { name: "Enable bucket", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.goto("/admin/s3");
  await expect(page.getByRole("region", { name: bucket })).toBeVisible();
  await page.getByLabel("Search buckets").fill(bucket);
  if (process.env.E2E_SCREENSHOT_DIR) {
    await mkdir(process.env.E2E_SCREENSHOT_DIR, { recursive: true });
    await page.screenshot({ path: join(process.env.E2E_SCREENSHOT_DIR, "s3-buckets-desktop.png") });
  }
  await page.getByRole("region", { name: bucket }).getByRole("button", { name: "Delete bucket", exact: true }).click();
  await page.getByRole("dialog", { name: "Delete bucket?", exact: true }).getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByRole("region", { name: bucket })).toHaveCount(0);
  expect((await page.request.get(`/api/nodes/${node}`)).ok()).toBe(true);
  expect(errors).toEqual([]);
});

test("personal folders never offer publishing and localized S3 settings fit a phone", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await signIn(page);
  const parent = await makeFolder(page, "Private S3 parent");
  const personal = await makeFolder(page, "Personal S3", parent);
  await page.goto(`/files/${parent}`);
  const info = await (await page.request.get(`/api/s3/buckets?node_id=${personal}`)).json();
  expect(info.can_publish).toBe(false);
  const row = page.getByRole("row").filter({ hasText: "Personal S3" });
  await row.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "Publish as S3 bucket…", exact: true })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.setViewportSize({ width: 390, height: 844 });
  for (const [lang, label] of [
    ["zh-TW", "啟用 S3 存取"],
    ["zh-CN", "启用 S3 访问"],
    ["ja", "S3 アクセスを有効化"],
  ]) {
    await page.evaluate((language) => localStorage.setItem("tf-lang", language), lang);
    await page.context().addCookies([
      { name: "tf_lang", value: lang, url: new URL(page.url()).origin },
      { name: "tf_lang_chosen", value: "1", url: new URL(page.url()).origin },
    ]);
    await page.goto("/admin/general");
    await expect(page.getByRole("checkbox", { name: label, exact: true })).toBeVisible();
    await page
      .locator("form")
      .filter({ has: page.getByRole("heading", { name: "S3", exact: true }) })
      .scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    if (lang === "zh-TW" && process.env.E2E_SCREENSHOT_DIR) await page.screenshot({ path: join(process.env.E2E_SCREENSHOT_DIR, "s3-settings-zh-TW-mobile.png") });
  }
  expect(errors).toEqual([]);
});
