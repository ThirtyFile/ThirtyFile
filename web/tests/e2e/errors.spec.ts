// The error log against the real server: errors the page reports, failed requests the server records, the two joined
// by request id, and the log administrators read (Control panel > Activity > Errors)
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { signIn } from "./helpers";

interface Entry {
  id: number;
  source: string;
  severity: string;
  kind: string;
  operation: string;
  message: string;
  status: number | null;
  request_id: string | null;
  client: string;
  username: string;
  count: number;
}

async function errors(request: APIRequestContext, q: string): Promise<Entry[]> {
  const res = await request.get(`/api/admin/errors?q=${encodeURIComponent(q)}`);
  expect(res.ok()).toBe(true);
  return (await res.json()).items;
}

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

test("uncaught errors and rejected promises of the page are recorded", async ({ page }) => {
  await signIn(page);
  const tag = Date.now().toString(36);
  await page.evaluate((tag) => {
    setTimeout(() => {
      throw new Error(`Uncaught e2e ${tag}`);
    });
    void Promise.reject(new Error(`Rejected e2e ${tag}`));
  }, tag);
  await expect.poll(async () => (await errors(page.request, tag)).map((e) => `${e.source} ${e.kind} ${e.message}`).sort()).toEqual([
    `frontend rejection Rejected e2e ${tag}`,
    `frontend uncaught Uncaught e2e ${tag}`,
  ]);
  const [e] = await errors(page.request, `Uncaught e2e ${tag}`);
  expect(e.username).toBe("admin");
});

test("a failed upload shown to the person is recorded with the server's request id", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Upload errors");
  const request = `e2e${Date.now().toString(36)}`;
  // The server refuses the upload (the answer is made up here: a refusal isn't retried, so the failure shows at once)
  await page.route("**/api/uploads", (route) =>
    route.request().method() === "POST"
      ? route.fulfill({
          status: 403,
          contentType: "application/json",
          headers: { "x-request-id": request },
          body: JSON.stringify({ error: "You no longer have permission to upload files" }),
        })
      : route.continue(),
  );
  await page.goto(`/files/${dir}`);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name: "big.bin", mimeType: "application/octet-stream", buffer: Buffer.from("x") }]);
  await expect(page.getByText("You no longer have permission to upload files").first()).toBeVisible();
  await expect.poll(async () => (await errors(page.request, request)).map((e) => `${e.kind} ${e.operation} ${e.status} ${e.severity}`)).toEqual([
    "handled upload 403 warning",
  ]);
});

test("a refused rename is one incident: the server's record with what the page showed", async ({ page }) => {
  await signIn(page);
  const dir = await folder(page, "Rename errors");
  await emptyFile(page, dir, "first.txt");
  await emptyFile(page, dir, "second.txt");
  await page.goto(`/files/${dir}`);
  const grid = page.getByRole("grid");
  await grid.getByRole("row").filter({ hasText: "second.txt" }).click();
  await page.keyboard.press("F2");
  const input = grid.getByRole("textbox");
  await input.fill("first.txt");
  await input.press("Enter");
  await expect(page.getByText('"first.txt" already exists')).toBeVisible();
  await expect
    .poll(async () => (await errors(page.request, "PATCH /api/nodes/{id}")).find((e) => e.client.startsWith("rename:"))?.kind)
    .toBe("conflict");
  const entries = (await errors(page.request, "PATCH /api/nodes/{id}")).filter((e) => e.client.startsWith("rename:"));
  const e = entries[0];
  expect([e.source, e.severity, e.status]).toEqual(["backend", "warning", 409]);
  // The name is left out of what the server and the page recorded
  expect([e.message, e.client]).toEqual(['"…" already exists', 'rename: "…" already exists']);
  // No second record of the same incident from the page
  expect(await errors(page.request, e.request_id!)).toHaveLength(1);
});

test("administrators read the error log as text; other people can't", async ({ page, playwright, baseURL }) => {
  await signIn(page);
  const tag = Date.now().toString(36);
  const markup = `<img src=x onerror="window.injected=1"> ${tag}`;
  expect((await page.request.post("/api/client-errors", { data: { kind: "uncaught", message: markup } })).status()).toBe(202);
  await expect.poll(async () => (await errors(page.request, tag)).length).toBe(1);

  await page.goto("/admin/activity");
  await page.getByRole("tab", { name: "Errors" }).click();
  await page.getByPlaceholder("Search messages or request IDs").fill(tag);
  // Names in quotes are left out of what is recorded; the rest shows as text, never as markup
  const row = page.getByRole("row").filter({ hasText: tag });
  await expect(row).toContainText('<img src=x onerror="…">');
  expect(await page.evaluate(() => (window as { injected?: number }).injected)).toBeUndefined();
  await row.locator("button[aria-expanded]").click();
  await expect(page.getByText("Script error")).toBeVisible();
  await expect(page.getByText("Request ID")).toHaveCount(0);

  // Someone who isn't an administrator
  const name = `err-${tag}`;
  expect((await page.request.post("/api/admin/users", { data: { username: name, password: "a-long-test-password-1" } })).ok()).toBe(true);
  const other = await playwright.request.newContext({ baseURL });
  expect((await other.post("/api/auth/login", { data: { username: name, password: "a-long-test-password-1" } })).ok()).toBe(true);
  expect((await other.put("/api/auth/password", { data: { current: "a-long-test-password-1", new: "another-long-password-2" } })).ok()).toBe(true);
  const res = await other.get("/api/admin/errors");
  expect(res.status()).toBe(403);
  await other.dispose();
});
