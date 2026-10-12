// Moving a file to another folder, deleting it, and restoring it from the trash, the way people do it in the page; and
// restoring a file whose folder is in the trash too, then that folder. As an account of its own: the administrator's
// trash holds thousands of items from the other tests.
import { expect, test } from "@playwright/test";
import { answer, makeFolder, signInAsNewUser, uploadFile } from "./helpers";

test("a file moved with Move to…, deleted, and restored from the trash is back where it was moved", async ({ page }) => {
  await signInAsNewUser(page, "trash");
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
  // Each change waits for the server's answer: it may wait its turn behind other tests' changes
  const moved = answer(page, "POST", "/api/nodes/move");
  await picker.getByRole("button", { name: "Move here" }).click();
  expect((await moved).ok()).toBe(true);
  await expect(row("report.txt")).toHaveCount(0);
  await row("Archive").dblclick();
  await page.waitForURL(`**/files/${archive}`);
  await expect(row("report.txt")).toBeVisible();

  // Delete: to the trash, after a question
  await row("report.txt").click();
  await page.keyboard.press("Delete");
  const trashed = answer(page, "POST", "/api/nodes/trash");
  await page.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  await expect(row("report.txt")).toHaveCount(0);

  // Restore from the trash: back in Archive
  await page.goto("/trash");
  await row("report.txt").click();
  const restored = answer(page, "POST", "/api/trash/restore");
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  expect((await restored).ok()).toBe(true);
  await expect(page.getByText("Restored 1 item")).toBeVisible();
  const info = await (await page.request.get(`/api/nodes/${id}`)).json();
  expect([info.node.parent_id, info.node.trashed_at]).toEqual([archive, null]);
  await page.goto(`/files/${archive}`);
  await expect(row("report.txt")).toBeVisible();
});

test("a file whose folder was deleted after it goes back into that folder, made again, and the folder is merged into it", async ({ page }) => {
  await signInAsNewUser(page, "trash");
  const dir = await makeFolder(page, "Restore into its folder");
  const abc = await makeFolder(page, "ABC", dir);
  const txt = await uploadFile(page, abc, "ABC.txt", "a");
  await uploadFile(page, abc, "other.txt", "b");
  for (const id of [txt, abc]) expect((await page.request.post("/api/nodes/trash", { data: { ids: [id] } })).ok()).toBe(true);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  const row = (name: string) => page.locator("[data-node-id]").filter({ has: page.getByText(name, { exact: true }) });
  const names = async (folder: string) => ((await (await page.request.get(`/api/nodes/${folder}/children`)).json()) as { name: string; id: string }[]).map((n) => n.name).sort();

  // The file first: a new folder ABC holds it, the one deleted stays in the trash
  await page.goto("/trash");
  await row("ABC.txt").click();
  const restored = answer(page, "POST", "/api/trash/restore");
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  expect((await restored).ok()).toBe(true);
  await expect(row("ABC.txt")).toHaveCount(0);
  expect(await names(dir)).toEqual(["ABC"]);
  const made = ((await (await page.request.get(`/api/nodes/${dir}/children`)).json()) as { id: string }[])[0].id;
  expect(made).not.toBe(abc);
  expect(await names(made)).toEqual(["ABC.txt"]);

  // Then the folder: the page asks, and it is merged into the one made again
  await row("ABC").click();
  const merged = answer(page, "POST", "/api/trash/restore");
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  const dialog = page.getByRole("dialog", { name: 'The destination already has a folder named "ABC"' });
  await dialog.getByRole("button", { name: "Merge the folders, keeping both files when names clash" }).click();
  expect((await merged).ok()).toBe(true);
  await expect(row("ABC")).toHaveCount(0);
  expect(await names(dir)).toEqual(["ABC"]);
  expect(await names(made)).toEqual(["ABC.txt", "other.txt"]);
});
