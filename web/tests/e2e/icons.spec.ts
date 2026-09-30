// File icons by format, language and tool in the real interface: the file list's views, both themes, and the upload panel
import { expect, test, type Page } from "@playwright/test";
import { signIn } from "./helpers";

async function folder(page: Page, name: string): Promise<string> {
  const root = (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: root, name: `${name} ${Date.now().toString(36)}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

async function emptyFile(page: Page, parent: string, name: string) {
  const b64 = (s: string) => Buffer.from(s).toString("base64");
  const res = await page.request.post("/api/uploads", {
    headers: { "Tus-Resumable": "1.0.0", "Upload-Length": "0", "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parent)}` },
  });
  expect(res.ok()).toBe(true);
}

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
    const dir = await folder(page, `Icons ${theme}`);
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
  const dir = await folder(page, "Icons upload");
  await page.goto(`/files/${dir}`);
  await page.locator('input[type="file"][multiple]').setInputFiles([
    { name: "script.py", mimeType: "text/x-python", buffer: Buffer.from("print(1)\n") },
    { name: "Makefile", mimeType: "", buffer: Buffer.from("all:\n") },
  ]);
  await expect(page.getByRole("status").filter({ hasText: "2 uploads complete" })).toBeAttached();
  const panel = page.locator(".rounded-xl").filter({ has: page.getByText("2 uploads complete", { exact: true }).first() });
  await expect(panel.locator('svg[data-type="python"]')).toBeVisible();
  await expect(panel.locator('svg[data-type="build"]')).toBeVisible();
});
