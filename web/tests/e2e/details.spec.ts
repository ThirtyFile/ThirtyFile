// The details pane while moving through a folder, share links put on the clipboard, and restoring skipped items.
import { expect, test, type Page } from "@playwright/test";
import { signIn } from "./helpers";

/** A new folder in My files, with a unique name */
async function folder(page: Page, name: string): Promise<string> {
  const me = await (await page.request.get("/api/auth/me")).json();
  const res = await page.request.post("/api/folders", { data: { parent_id: me.root_id, name: `${name} ${Date.now().toString(36)}` } });
  return (await res.json()).id;
}

/** An empty file (a tus upload of no bytes is complete at once); returns its id */
async function file(page: Page, parent: string, name: string): Promise<string> {
  const b64 = (s: string) => Buffer.from(s).toString("base64");
  const res = await page.request.post("/api/uploads", {
    headers: { "Tus-Resumable": "1.0.0", "Upload-Length": "0", "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parent)}` },
  });
  expect(res.ok()).toBe(true);
  return res.headers()["x-node-id"];
}

test("holding an arrow key in the list asks for the details of where it stops, not of every file passed", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Details");
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
  const dir = await folder(page, "Share");
  await file(page, dir, "shared.txt");
  await page.goto(`/files/${dir}`);
  const row = page.locator("[data-node-id]").filter({ hasText: "shared.txt" });
  await row.click({ button: "right" });
  await page
    .getByRole("menuitem", { name: /share link/i })
    .first()
    .click();
  await page.getByRole("button", { name: "Create link" }).click();
  await expect(page.getByText("Share link created and copied to clipboard")).toBeVisible();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toMatch(/\/share\/[A-Za-z0-9_-]+$/);

  // A browser that refuses: the link is made, but not said to be copied, and Copy link is offered
  await page.evaluate(() => {
    navigator.clipboard.write = () => Promise.reject(new Error("refused"));
    navigator.clipboard.writeText = () => Promise.reject(new Error("refused"));
    document.execCommand = () => false;
  });
  await page.getByRole("button", { name: "Create link" }).click();
  await expect(page.getByText("Share link created", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Copy link" }).last()).toBeVisible();
});

test("restoring an item whose name was taken meanwhile and skipping it doesn't say it was restored", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Restore");
  const first = await file(page, dir, "same.txt");
  expect((await page.request.post("/api/nodes/trash", { data: { ids: [first] } })).ok()).toBe(true);
  await file(page, dir, "same.txt");
  await page.goto("/trash");
  await page.locator("[data-node-id]").filter({ hasText: "same.txt" }).first().click();
  await page.getByRole("button", { name: "Restore", exact: true }).first().click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: /Skip this file/ })
    .click();
  await expect(page.getByText("Nothing was restored: every item was skipped")).toBeVisible();
  await expect(page.getByText(/Restored 0 items/)).toHaveCount(0);
});
