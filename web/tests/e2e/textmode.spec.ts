// Opening a file of an unknown kind in the text editor from the preview screen, against the real server
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, signIn, uploadFile } from "./helpers";

const content = async (page: Page, id: string) => (await page.request.get(`/api/files/${id}/content`)).text();

test("a Dockerfile opens in the text editor from the preview screen and saves without being renamed", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Text mode");
  const id = await uploadFile(page, dir, "Dockerfile", Buffer.from("FROM node:22\r\n"));
  const before = await (await page.request.get(`/api/nodes/${id}`)).json();
  await page.goto(`/view/${id}`);
  await expect(page.getByText("Preview isn't available for this file type")).toBeVisible();
  await page.getByRole("button", { name: "Open in text editor" }).click();
  const editor = page.locator(".cm-content");
  await expect(editor).toHaveText("FROM node:22");
  await expect(page.getByText("UTF-8", { exact: true })).toBeVisible();
  await editor.click();
  await page.keyboard.press("Control+End");
  // (the file ends with a line break: the cursor is on the empty last line)
  await page.keyboard.type("RUN pnpm install");
  // Saved once the server has answered: it may wait its turn behind other tests' changes
  const saved = answer(page, "PUT", `/api/files/${id}/content`);
  await page.keyboard.press("Control+s");
  expect((await saved).ok()).toBe(true);
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  // Windows line endings are kept, and the name and type are unchanged
  expect(await content(page, id)).toBe("FROM node:22\r\nRUN pnpm install");
  const after = await (await page.request.get(`/api/nodes/${id}`)).json();
  expect([after.node.name, after.node.mime]).toEqual([before.node.name, before.node.mime]);

  // Unsaved edits are kept when going back to the usual view, and open again with the editor
  await editor.click();
  await page.keyboard.type("\n# draft");
  await page.getByRole("button", { name: "Default view" }).click();
  await expect(page.getByText("Your unsaved changes are kept in the text editor.")).toBeVisible();
  await page.getByRole("button", { name: "Open in text editor" }).click();
  await expect(editor).toContainText("# draft");
});

test("a binary file opened as text is read-only, and a large one isn't loaded", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Text mode binary");
  const binary = await uploadFile(page, dir, "blob.customext", Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]));
  await page.goto(`/view/${binary}`);
  await page.getByRole("button", { name: "Open in text editor" }).click();
  await expect(page.getByText("This file doesn't look like text: opened read-only so it isn't damaged")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Save/ })).toHaveCount(0);

  const big = await uploadFile(page, dir, "huge.customext", Buffer.alloc(5 * 1024 * 1024 + 1, 0x61));
  let fetched = false;
  page.on("request", (r) => {
    if (r.url().includes(`/api/files/${big}/content`)) fetched = true;
  });
  await page.goto(`/view/${big}`);
  await expect(page.getByText(/too large to open in the text editor/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Open in text editor" })).toHaveCount(0);
  expect(fetched).toBe(false);
});

test("a share link shows files as text read-only, and only when it allows downloading", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Text mode share");
  await uploadFile(page, dir, "README", Buffer.from("Read me first\n"));
  const link = async (allow_download: boolean) => (await (await page.request.post("/api/shares", { data: { node_id: dir, allow_download } })).json()).id;

  const viewOnly = await link(false);
  await page.goto(`/share/${viewOnly}`);
  await page.getByText("README", { exact: true }).dblclick();
  await expect(page.getByText("Preview isn't available for this file type")).toBeVisible();
  await expect(page.getByRole("button", { name: "Open in text editor" })).toHaveCount(0);

  const withDownload = await link(true);
  await page.goto(`/share/${withDownload}`);
  await page.getByText("README", { exact: true }).dblclick();
  await page.getByRole("button", { name: "Open in text editor" }).click();
  await expect(page.locator(".cm-content")).toHaveText("Read me first");
  await expect(page.getByText("· Read-only")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Save/ })).toHaveCount(0);
});
