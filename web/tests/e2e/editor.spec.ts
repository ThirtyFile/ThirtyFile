// Text editor and tabs against the real server: edits typed while a save is on its way, and closing other tabs
import { expect, test, type Page } from "@playwright/test";

const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

async function signIn(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "Click or press any key to sign in" }).click();
  await page.getByLabel("Username").fill("admin");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(/\/files/);
}

/** A folder of the test's own in My files (unique, so a retry on the same server starts afresh) */
async function folder(page: Page, name: string): Promise<string> {
  const root = (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: root, name: `${name} ${Date.now().toString(36)}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

/** An empty file, made the way "New > Text document" makes one */
async function emptyFile(page: Page, parent: string, name: string): Promise<string> {
  const b64 = (s: string) => Buffer.from(s).toString("base64");
  const res = await page.request.post("/api/uploads", {
    headers: { "Tus-Resumable": "1.0.0", "Upload-Length": "0", "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parent)}` },
  });
  expect(res.ok()).toBe(true);
  return res.headers()["x-node-id"];
}

const content = async (page: Page, id: string) => (await page.request.get(`/api/files/${id}/content`)).text();

test("edits typed while a text file is being saved stay unsaved, and survive switching tabs", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Editor");
  const id = await emptyFile(page, dir, "notes.txt");
  // Saving takes a while
  await page.route(`**/api/files/${id}/content`, async (route) => {
    if (route.request().method() === "PUT") await new Promise((r) => setTimeout(r, 1500));
    await route.continue();
  });
  await page.goto(`/view/${id}`);
  const editor = page.locator(".cm-content");
  const closeButton = page.getByRole("button", { name: "Close notes.txt" });
  await editor.click();
  await page.keyboard.type("A");
  await page.keyboard.press("Control+s");
  await page.keyboard.type("B");
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(await content(page, id)).toBe("A");
  // Still unsaved: the tab's marker stays
  await expect(editor).toHaveText("AB");
  await expect(closeButton).toHaveAttribute("title", "Unsaved changes");

  // Another tab and back: the newer text is still there
  await page.getByRole("button", { name: "New tab" }).click();
  await page.waitForURL(/\/files$/);
  await page.getByRole("button", { name: "Close notes.txt" }).locator("..").click();
  await page.waitForURL(`**/view/${id}`);
  await expect(editor).toHaveText("AB");
  await expect(closeButton).toHaveAttribute("title", "Unsaved changes");

  // The next save goes through without a conflict, and the tab is no longer marked
  await editor.click();
  await page.keyboard.press("Control+s");
  await expect.poll(() => content(page, id)).toBe("AB");
  await expect(closeButton).toHaveAttribute("title", "Close tab");
});

test("closing other tabs from a tab that isn't shown shows that tab's page", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Tabs");
  const id = await emptyFile(page, dir, "open.txt");
  await page.goto(`/files/${dir}`);
  await expect(page.locator("[data-node-id]").filter({ hasText: "open.txt" })).toBeVisible();

  // A second tab with the file open in its editor, left active
  await page.getByRole("button", { name: "New tab" }).click();
  await page.waitForURL(/\/files$/);
  await page.goto(`/view/${id}`);
  await expect(page.locator(".cm-content")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Close / })).toHaveCount(2);

  // Close other tabs, from the folder's tab
  const folderTab = page.getByRole("button", { name: /^Close Tabs / }).locator("..");
  await folderTab.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Close other tabs" }).click();
  await page.waitForURL(`**/files/${dir}`);
  await expect(page.getByRole("button", { name: /^Close / })).toHaveCount(1);
  await expect(page.locator("[data-node-id]").filter({ hasText: "open.txt" })).toBeVisible();
  await expect(page.locator(".cm-content")).toHaveCount(0);
});
