// Exported logs: the page writes the CSV file from the records the server sends, with the names it shows.
import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn } from "./helpers";

async function exported(page: Page): Promise<{ name: string; text: string }> {
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export CSV" }).filter({ visible: true }).click()]);
  return { name: download.suggestedFilename(), text: await readFile(await download.path(), "utf8") };
}

test("the activity log and the sign-in log export with the names the page shows", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "=Exported");
  const share = await page.request.post("/api/shares", { data: { node_id: folder, expires_at: Math.floor(Date.now() / 1000) + 86_400, allow_upload: true } });
  expect(share.ok()).toBe(true);

  await page.goto("/admin/activity");
  await expect(page.locator("tbody tr").first()).toBeVisible();
  const activity = await exported(page);
  expect(activity.name).toMatch(/^activity-log-\d{8}\.csv$/);
  const lines = activity.text.split("\n");
  expect(lines[0]).toBe("\uFEFFTime,User,Action,Item,Details,Space");
  // The action's name, not its code; a name a spreadsheet would run is kept as text
  const made = lines.find((l) => l.includes("Create folder,'=Exported"));
  expect(made).toMatch(/^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,admin,Create folder,'=Exported /);
  expect(lines.some((l) => /,Create share link,'=Exported [^,]*,"Expires \d{4}-\d\d-\d\d, accepts files",/.test(l))).toBe(true);
  expect(activity.text).not.toContain("create_folder");

  await page.getByRole("tab", { name: "Sign-in log" }).click();
  const logins = await exported(page);
  expect(logins.name).toMatch(/^login-log-\d{8}\.csv$/);
  expect(logins.text.split("\n")[0]).toBe("\uFEFFTime,Account,Event,Method,IP,Browser");
  expect(logins.text).toMatch(/,admin,Signed in,Password,/);
});
