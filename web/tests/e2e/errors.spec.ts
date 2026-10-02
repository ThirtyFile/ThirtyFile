// The error log against the real server: errors the page reports, failed requests the server records, the two joined
// by request id, and the log administrators read (Control panel > Activity > Errors)
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { answer, makeFolder, makeUser, openFolder, signIn, unique, uploadFile, USER_PASSWORD } from "./helpers";

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

/**
 * Waits until `check` passes on the records the server has written. The server writes them in the background, each
 * batch waiting its turn behind other changes (another test's large change takes up to half a minute): there is no
 * answer to wait for, so this tries again for as long as the test may run.
 */
const recorded = (check: () => Promise<void>) => expect(check).toPass();

const emptyFile = (page: Page, parent: string, name: string) => uploadFile(page, parent, name, "");

test("uncaught errors and rejected promises of the page are recorded", async ({ page }) => {
  await signIn(page);
  const tag = unique();
  await page.evaluate((tag) => {
    queueMicrotask(() => {
      throw new Error(`Uncaught e2e ${tag}`);
    });
    void Promise.reject(new Error(`Rejected e2e ${tag}`));
  }, tag);
  await recorded(async () =>
    expect((await errors(page.request, tag)).map((e) => `${e.source} ${e.kind} ${e.message}`).sort()).toEqual([
      `frontend rejection Rejected e2e ${tag}`,
      `frontend uncaught Uncaught e2e ${tag}`,
    ]),
  );
  const [e] = await errors(page.request, `Uncaught e2e ${tag}`);
  expect(e.username).toBe("admin");
});

test("a failed upload shown to the person is recorded with the server's request id", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Upload errors");
  const request = `e2e${unique()}`;
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
  await openFolder(page, dir);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name: "big.bin", mimeType: "application/octet-stream", buffer: Buffer.from("x") }]);
  await expect(page.getByText("You no longer have permission to upload files").first()).toBeVisible();
  await recorded(async () => expect((await errors(page.request, request)).map((e) => `${e.kind} ${e.operation} ${e.status} ${e.severity}`)).toEqual(["handled upload 403 warning"]));
});

test("a refused rename is one incident: the server's record with what the page showed", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Rename errors");
  await emptyFile(page, dir, "first.txt");
  await emptyFile(page, dir, "second.txt");
  await page.goto(`/files/${dir}`);
  const grid = page.getByRole("grid");
  await grid.getByRole("row").filter({ hasText: "second.txt" }).click();
  await page.keyboard.press("F2");
  const input = grid.getByRole("textbox");
  await input.fill("first.txt");
  // This test's refusal, by its request id: other tests' renames are refused at the same time
  const refused = answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);
  await input.press("Enter");
  const res = await refused;
  expect(res.status()).toBe(409);
  const id = res.headers()["x-request-id"];
  expect(id).toBeTruthy();
  await expect(page.getByText('"first.txt" already exists')).toBeVisible();
  // One record, of the server, with what the page showed: no second record of the same incident from the page
  await recorded(async () => expect((await errors(page.request, id)).map((e) => e.client)).toEqual(['rename: "…" already exists']));
  const [e] = await errors(page.request, id);
  expect([e.source, e.severity, e.status, e.kind, e.operation]).toEqual(["backend", "warning", 409, "conflict", "PATCH /api/nodes/{id}"]);
  // The name is left out of what the server and the page recorded
  expect(e.message).toBe('"…" already exists');
});

test("administrators read the error log as text; other people can't", async ({ page, playwright, baseURL }) => {
  await signIn(page);
  const tag = unique();
  const markup = `<img src=x onerror="window.injected=1"> ${tag}`;
  expect((await page.request.post("/api/client-errors", { data: { kind: "uncaught", message: markup } })).status()).toBe(202);
  await recorded(async () => expect(await errors(page.request, tag)).toHaveLength(1));

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
  const name = await makeUser(page, "err");
  const other = await playwright.request.newContext({ baseURL });
  expect((await other.post("/api/auth/login", { data: { username: name, password: USER_PASSWORD } })).ok()).toBe(true);
  expect((await other.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  const res = await other.get("/api/admin/errors");
  expect(res.status()).toBe(403);
  await other.dispose();
});
