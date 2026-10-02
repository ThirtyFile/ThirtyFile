// The details pane while moving through a folder, share links put on the clipboard, and restoring skipped items.
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, signIn, signInAsNewUser, uploadFile } from "./helpers";

/** An empty file: its id */
const file = (page: Page, parent: string, name: string) => uploadFile(page, parent, name, "");

test("holding an arrow key in the list asks for the details of where it stops, not of every file passed", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Details");
  for (let i = 0; i < 20; i++) await file(page, dir, `file ${String(i).padStart(2, "0")}.txt`);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${dir}`);
  await page.getByRole("button", { name: "Details pane" }).click();
  const pane = page.getByRole("complementary", { name: "Details pane" });
  await page.locator("[data-node-id]").filter({ hasText: "file 00.txt" }).click();
  await expect(pane.getByText("file 00.txt")).toBeVisible();

  const asked: string[] = [];
  page.on("request", (r) => {
    if (/\/api\/nodes\/[^/]+\/activity/.test(r.url())) asked.push(r.url());
  });
  for (let i = 0; i < 19; i++) await page.keyboard.press("ArrowDown", { delay: 20 });
  await expect(pane.getByText("file 19.txt")).toBeVisible();
  // The last file's details load once the selection stays put
  await expect(pane.getByRole("region", { name: "Activity" }).getByRole("listitem")).toBeVisible();
  expect(asked.length).toBeLessThanOrEqual(2);
});

test("a new share link is said to be copied only when it is on the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await signIn(page);
  const dir = await makeFolder(page, "Share");
  await file(page, dir, "shared.txt");
  await page.goto(`/files/${dir}`);
  const row = page.locator("[data-node-id]").filter({ hasText: "shared.txt" });
  await row.click({ button: "right" });
  await page
    .getByRole("menuitem", { name: /share link/i })
    .first()
    .click();
  // Each link is made once the server has answered: it may wait its turn behind other tests' changes
  let created = answer(page, "POST", "/api/shares");
  await page.getByRole("button", { name: "Create link" }).click();
  expect((await created).ok()).toBe(true);
  await expect(page.getByText("Share link created and copied to clipboard")).toBeVisible();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toMatch(/\/share\/[A-Za-z0-9_-]+$/);

  // A browser that refuses: the link is made, but not said to be copied, and Copy link is offered
  await page.evaluate(() => {
    navigator.clipboard.write = () => Promise.reject(new Error("refused"));
    navigator.clipboard.writeText = () => Promise.reject(new Error("refused"));
    document.execCommand = () => false;
  });
  created = answer(page, "POST", "/api/shares");
  await page.getByRole("button", { name: "Create link" }).click();
  expect((await created).ok()).toBe(true);
  await expect(page.getByText("Share link created", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Copy link" }).last()).toBeVisible();
});

test("restoring an item whose name was taken meanwhile and skipping it doesn't say it was restored", async ({ page }) => {
  // As an account of its own: the administrator's trash holds thousands of items from the other tests
  await signInAsNewUser(page, "restore");
  const dir = await makeFolder(page, "Restore");
  const first = await file(page, dir, "same.txt");
  expect((await page.request.post("/api/nodes/trash", { data: { ids: [first] } })).ok()).toBe(true);
  await file(page, dir, "same.txt");
  await page.goto("/trash");
  await page.locator("[data-node-id]").filter({ hasText: "same.txt" }).click();
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: /Skip this file/ })
    .click();
  await expect(page.getByText("Nothing was restored: every item was skipped")).toBeVisible();
  await expect(page.getByText(/Restored 0 items/)).toHaveCount(0);
});
