// Sharing a folder by link, seen by a visitor who isn't signed in: the password, browsing, downloading, and the link
// stopping once it is deleted
import { readFile } from "node:fs/promises";
import { devices, expect, test } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

test("a visitor unlocks a shared folder with its password, opens a folder in it and downloads a file", async ({ page, browser, baseURL }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Shared by link");
  const sub = await makeFolder(page, "Minutes", dir);
  await uploadFile(page, dir, "agenda.txt", "Agenda for Monday");
  await uploadFile(page, sub, "2026-09.txt", "Minutes of September");
  const res = await page.request.post("/api/shares", { data: { node_id: dir, password: "a-share-password-1" } });
  expect(res.ok()).toBe(true);
  const link = (await res.json()).id as string;

  // Another browser, signed in nowhere
  const visitor = await browser.newContext({ baseURL, acceptDownloads: true });
  const guest = await visitor.newPage();
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText("This share requires a password")).toBeVisible();
  await guest.getByLabel("Password").fill("not the password");
  await guest.getByRole("button", { name: "Open", exact: true }).click();
  await expect(guest.getByRole("alert")).toBeVisible();
  await guest.getByLabel("Password").fill("a-share-password-1");
  await guest.getByRole("button", { name: "Open", exact: true }).click();

  const row = (name: string) => guest.locator("[data-node-id]").filter({ hasText: name });
  await expect(row("agenda.txt")).toBeVisible();
  await expect(guest.getByText("Shared by admin")).toBeVisible();
  await row("Minutes").dblclick();
  await expect(row("2026-09.txt")).toBeVisible();

  await row("2026-09.txt").click();
  const downloading = guest.waitForEvent("download");
  await guest.getByRole("button", { name: "Download 1 item" }).first().click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("2026-09.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("Minutes of September");

  // Deleted by its owner, the link no longer opens
  expect((await page.request.delete(`/api/shares/${link}`)).ok()).toBe(true);
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText(/doesn't exist or has expired/)).toBeVisible();
  await visitor.close();
});

test("a link to one file shows it to a visitor on a phone, who can download it", async ({ page, browser, baseURL }) => {
  await signIn(page);
  const dir = await makeFolder(page, "One file");
  const id = await uploadFile(page, dir, "notes.txt", "Just this file");
  const res = await page.request.post("/api/shares", { data: { node_id: id } });
  expect(res.ok()).toBe(true);
  const link = (await res.json()).id as string;

  const visitor = await browser.newContext({ ...devices["Pixel 7"], baseURL, acceptDownloads: true });
  const guest = await visitor.newPage();
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText("notes.txt").first()).toBeVisible();
  // Nothing wider than the phone's screen
  expect(await guest.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const downloading = guest.waitForEvent("download");
  await guest.getByRole("button", { name: "Download", exact: true }).first().click();
  const download = await downloading;
  expect(await readFile(await download.path(), "utf8")).toBe("Just this file");
  await visitor.close();
});

test("Share with… picks a person with the keyboard alone", async ({ page }) => {
  await signIn(page);
  const kim = `kim-${Date.now().toString(36)}`;
  expect((await page.request.post("/api/admin/users", { data: { username: kim, password: "a-long-test-password-1" } })).ok()).toBe(true);
  const dir = await makeFolder(page, "Shared with Kim");
  const me = await (await page.request.get("/api/auth/me")).json();
  await page.goto(`/files/${me.root_id}`);
  await page.locator("[data-node-id]").filter({ hasText: "Shared with Kim" }).first().click();
  await page.getByRole("button", { name: "Share with…" }).click();
  const dialog = page.getByRole("dialog");
  const box = dialog.getByRole("combobox", { name: "Username or group name" });
  await box.focus();
  await page.keyboard.type(kim);
  await expect(box).toHaveAttribute("aria-expanded", "true");
  const option = dialog.getByRole("option", { name: new RegExp(kim) });
  await expect(option).toBeVisible();
  // The arrows go through what was found; the box keeps the focus and says which one they are on
  await page.keyboard.press("ArrowDown");
  await expect(box).toBeFocused();
  await expect(option).toHaveAttribute("aria-selected", "true");
  await expect(box).toHaveAttribute("aria-activedescendant", (await option.getAttribute("id"))!);
  // Escape closes the list, not the dialog
  await page.keyboard.press("Escape");
  await expect(box).toHaveAttribute("aria-expanded", "false");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(option).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  // Picked: the focus moves on to the role, then Add
  await expect(dialog.getByRole("combobox", { name: "Role" }).first()).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await expect(dialog.getByRole("button", { name: "Add" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect.poll(async () => JSON.stringify(await (await page.request.get(`/api/nodes/${dir}/access`)).json())).toContain(kim);
});
