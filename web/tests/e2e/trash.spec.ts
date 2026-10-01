// Moving a file to another folder, deleting it, and restoring it from the trash, the way people do it in the page
import { expect, test } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

test("a file moved with Move to…, deleted, and restored from the trash is back where it was moved", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Move and restore");
  const archive = await makeFolder(page, "Archive", dir);
  const id = await uploadFile(page, dir, "report.txt", "Quarterly report");
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${dir}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });

  // Move to… a folder picked in the dialog
  await row("report.txt").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Move to…" }).click();
  const picker = page.getByRole("dialog");
  await picker.getByRole("button", { name: "Archive" }).click();
  await picker.getByRole("button", { name: "Move here" }).click();
  await expect(row("report.txt")).toHaveCount(0);
  await row("Archive").dblclick();
  await page.waitForURL(`**/files/${archive}`);
  await expect(row("report.txt")).toBeVisible();

  // Delete: to the trash, after a question
  await row("report.txt").click();
  await page.keyboard.press("Delete");
  await page.getByRole("button", { name: "Move to trash" }).click();
  await expect(row("report.txt")).toHaveCount(0);

  // Restore from the trash: back in Archive
  await page.goto("/trash");
  await row("report.txt").first().click();
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  await expect(page.getByText("Restored 1 item")).toBeVisible();
  const info = await (await page.request.get(`/api/nodes/${id}`)).json();
  expect([info.node.parent_id, info.node.trashed_at]).toEqual([archive, null]);
  await page.goto(`/files/${archive}`);
  await expect(row("report.txt")).toBeVisible();
});
