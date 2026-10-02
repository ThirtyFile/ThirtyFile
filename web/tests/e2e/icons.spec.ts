// File icons by format, language and tool in the real interface: the file list's views, both themes, and the upload panel
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, openFolder, signIn, uploadFile, uploadFinished } from "./helpers";

const emptyFile = (page: Page, parent: string, name: string) => uploadFile(page, parent, name, "");

const FILES: [string, string][] = [
  ["config.json", "json"],
  ["main.rs", "rust"],
  ["main.go", "go"],
  ["Dockerfile", "docker"],
  [".env", "env"],
  ["Cargo.lock", "lock"],
  ["go.mod", "manifest"],
];

for (const theme of ["light", "dark"] as const) {
  test(`formats and tools have their own icons in the Details and icon views (${theme} theme)`, async ({ page }) => {
    await page.addInitScript((theme) => localStorage.setItem("tf-theme", theme), theme);
    await signIn(page);
    const dir = await makeFolder(page, `Icons ${theme}`);
    for (const [name] of FILES) await emptyFile(page, dir, name);
    for (const view of ["list", "grid", "tiles"]) {
      await page.evaluate((view) => localStorage.setItem("tf-view", JSON.stringify(view)), view);
      await page.goto(`/files/${dir}`);
      for (const [name, type] of FILES) {
        const item = page.locator("[data-node-id]").filter({ hasText: name });
        await expect(item.locator(`svg[data-type="${type}"]`)).toBeVisible();
      }
    }
    // The label on the page reads against the theme's background
    await expect(page.locator('svg[data-type="rust"] text')).toHaveText("RS");
    // A file nothing knows keeps the plain file icon
    await emptyFile(page, dir, "notes.customext");
    await page.reload();
    await expect(page.locator("[data-node-id]").filter({ hasText: "notes.customext" }).locator("svg[data-type]")).toHaveCount(0);
  });
}

test("the upload panel shows the same icons", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Icons upload");
  await openFolder(page, dir);
  // Both saved once the server has answered: it may wait its turn behind other tests' changes
  const finished = uploadFinished(page, 2);
  await page.locator('input[type="file"][multiple]').setInputFiles([
    { name: "script.py", mimeType: "text/x-python", buffer: Buffer.from("print(1)\n") },
    { name: "Makefile", mimeType: "", buffer: Buffer.from("all:\n") },
  ]);
  await finished;
  await expect(page.getByRole("status").filter({ hasText: "2 uploads complete" })).toBeAttached();
  const panel = page.locator(".rounded-xl").filter({ has: page.getByText("2 uploads complete", { exact: true }).first() });
  await expect(panel.locator('svg[data-type="python"]')).toBeVisible();
  await expect(panel.locator('svg[data-type="build"]')).toBeVisible();
});
