import { test, expect } from "@playwright/test";
import { join } from "node:path";
import { mkdir } from "node:fs/promises";
import { PASSWORD, answer, makeFolder, signIn, unique } from "./helpers";

test.describe.configure({ mode: "serial" });

test("administrators configure receive folders and manage device credentials on desktop and mobile", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await signIn(page);
  await page.goto("/admin/general");
  const form = page.locator("form").filter({ has: page.getByRole("heading", { name: "FTP / FTPS", exact: true }) });
  await expect(form.getByRole("checkbox", { name: "Enable FTP receive folders" })).not.toBeChecked();
  await form.getByRole("checkbox", { name: "Enable FTP receive folders" }).check();
  const enabled = answer(page, "PUT", "/api/admin/ftp");
  await form.getByRole("button", { name: "Save", exact: true }).click();
  expect((await enabled).ok()).toBe(true);
  const drives = await (await page.request.get("/api/admin/drives")).json();
  const space = drives.find((drive: { kind: string }) => drive.kind === "company");
  const name = `Receiving ${unique()}`;
  const node = await makeFolder(page, name, space.root_id);
  await page.goto(`/files/${space.root_id}`);
  await page.getByRole("row").filter({ hasText: name }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Make FTP receive folder…", exact: true }).click();
  const folders = page.getByRole("dialog", { name: "FTP receive folders", exact: true });
  await folders.getByRole("button", { name: "Make FTP receive folder…", exact: true }).click();
  const create = page.getByRole("dialog", { name: "Make FTP receive folder…", exact: true });
  const device = `Device ${unique()}`;
  await create.getByLabel("Receive folder name").fill(device);
  await expect(create.getByRole("checkbox", { name: "Allow device to list files" })).not.toBeChecked();
  await expect(create.getByRole("checkbox", { name: "Allow device to download files" })).not.toBeChecked();
  await create.getByLabel("Your current password").fill(PASSWORD);
  await create.getByRole("button", { name: "Create", exact: true }).click();
  await expect(create.getByText("Save the device password now. It is shown only once.")).toBeVisible();
  expect(await create.getByLabel("Device username").inputValue()).toMatch(/^ftp-/);
  const secret = await create.getByLabel("Device password").inputValue();
  expect(secret.length).toBeGreaterThan(30);
  await create.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByLabel("Device password")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.goto("/admin/ftp");
  await page.getByLabel("Filter receive folders").fill(device);
  const card = page.getByRole("region", { name: device });
  await expect(card.getByText("Listing blocked", { exact: false })).toBeVisible();
  const info = await (await page.request.get(`/api/ftp/folders?node_id=${node}`)).json();
  expect(JSON.stringify(info)).not.toContain(secret);
  if (process.env.E2E_SCREENSHOT_DIR) {
    await mkdir(process.env.E2E_SCREENSHOT_DIR, { recursive: true });
    await page.screenshot({ path: join(process.env.E2E_SCREENSHOT_DIR, "ftp-folders-desktop.png"), animations: "disabled" });
  }
  await card.getByRole("button", { name: "Edit", exact: true }).click();
  const edit = page.getByRole("dialog", { name: "Edit FTP receive folder", exact: true });
  await edit.getByRole("checkbox", { name: "Allow device to list files" }).check();
  await edit.getByLabel("Your current password").fill(PASSWORD);
  await edit.getByRole("button", { name: "Save", exact: true }).click();
  await expect(edit).toBeHidden();
  await expect(card.getByText("Listing allowed", { exact: false })).toBeVisible();
  await page.setViewportSize({ width: 390, height: 844 });
  await card.getByRole("button", { name: "Rotate password", exact: true }).click();
  const rotate = page.getByRole("dialog", { name: "Rotate FTP password", exact: true });
  await expect(rotate).toBeVisible();
  expect(await rotate.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  if (process.env.E2E_SCREENSHOT_DIR) await page.screenshot({ path: join(process.env.E2E_SCREENSHOT_DIR, "ftp-rotate-mobile.png"), animations: "disabled" });
  await rotate.getByLabel("Your current password").fill(PASSWORD);
  await rotate.getByRole("button", { name: "Rotate password", exact: true }).click();
  await expect(rotate.getByLabel("Device password")).not.toHaveValue(secret);
  await rotate.getByRole("button", { name: "Done", exact: true }).click();
  await card.getByRole("button", { name: "Delete", exact: true }).click();
  const remove = page.getByRole("dialog", { name: "Delete FTP receive folder?", exact: true });
  await remove.getByLabel("Your current password").fill(PASSWORD);
  await remove.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(card).toHaveCount(0);
  expect((await page.request.get(`/api/nodes/${node}`)).ok()).toBe(true);
  expect(errors).toEqual([]);
});

test("localized receive folder settings fit a phone", async ({ page }) => {
  await signIn(page);
  await page.setViewportSize({ width: 390, height: 844 });
  for (const [lang, label] of [
    ["zh-TW", "啟用 FTP 接收資料夾"],
    ["zh-CN", "启用 FTP 接收文件夹"],
    ["ja", "FTP 受信フォルダーを有効にする"],
  ]) {
    await page.evaluate((language) => localStorage.setItem("tf-lang", language), lang);
    await page.context().addCookies([
      { name: "tf_lang", value: lang, url: new URL(page.url()).origin },
      { name: "tf_lang_chosen", value: "1", url: new URL(page.url()).origin },
    ]);
    await page.goto("/admin/general");
    const checkbox = page.getByRole("checkbox", { name: label, exact: true });
    await expect(checkbox).toBeVisible();
    await checkbox.scrollIntoViewIfNeeded();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    if (lang === "zh-TW" && process.env.E2E_SCREENSHOT_DIR) await page.screenshot({ path: join(process.env.E2E_SCREENSHOT_DIR, "ftp-settings-zh-TW-mobile.png"), animations: "disabled" });
  }
});
