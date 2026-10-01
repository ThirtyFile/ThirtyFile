// Uploads interrupted by a reload continue once their files are chosen again, against the real server
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { signIn } from "./helpers";

const MB = 1024 * 1024;
/** tus-js-client sends 32 MB per request (uploads.ts) */
const CHUNK = 32 * MB;

let dir: string;
test.beforeAll(() => {
  dir = mkdtempSync(join(tmpdir(), "thirtyfile-recovery-"));
});
test.afterAll(() => rmSync(dir, { recursive: true, force: true }));

/** A file on disk (the browser keeps its date, as when a person picks it) */
function diskFile(name: string, size: number, seed: number): string {
  const path = join(dir, name);
  const bytes = Buffer.alloc(size);
  for (let i = 0; i < size; i += 4) bytes.writeUInt32LE((i * 2654435761 + seed) >>> 0, Math.min(i, size - 4));
  writeFileSync(path, bytes);
  return path;
}

async function folder(page: Page, name: string): Promise<string> {
  const root = (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: root, name: `${name} ${Date.now().toString(36)}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

async function children(page: Page, id: string): Promise<{ id: string; name: string; size: number }[]> {
  const res = await (await page.request.get(`/api/nodes/${id}/children?limit=100`)).json();
  return Array.isArray(res) ? res : res.items;
}

const sha = (b: Buffer) => createHash("sha256").update(b).digest("hex");

/**
 * Starts uploading a file and stops it after its first 32 MB reached the server: the rest never gets through (the
 * connection drops), until the page is reloaded
 */
async function startAndHold(page: Page, path: string) {
  let held = false;
  await page.route("**/api/uploads/*", async (route) => {
    const r = route.request();
    if (r.method() === "PATCH" && r.headers()["upload-offset"] !== "0") {
      held = true;
      return route.abort();
    }
    await route.continue();
  });
  await page.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await expect.poll(() => held, { timeout: 30_000 }).toBe(true);
}

/** Reloads the page while the upload is stopped, then lets requests through again */
async function reload(page: Page) {
  await page.reload();
  await page.unrouteAll({ behavior: "ignoreErrors" });
}

/** The PATCH requests' starting offsets from now on */
function patchOffsets(page: Page) {
  const offsets: number[] = [];
  page.on("request", (r) => {
    if (r.method() === "PATCH" && r.url().includes("/api/uploads/")) offsets.push(Number(r.headers()["upload-offset"]));
  });
  return offsets;
}

test("after a reload, choosing the file again continues from what the server has", async ({ page }) => {
  await signIn(page);
  const target = await folder(page, "Recovery");
  const path = diskFile("big-resume.bin", 40 * MB, 1);
  await page.goto(`/files/${target}`);
  await startAndHold(page, path);
  await reload(page);

  const region = page.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toContainText("big-resume.bin");
  await expect(region).toContainText("Can continue");
  // Only a record of it is kept, never the content
  const kept = await page.evaluate(() =>
    Object.entries(localStorage)
      .filter(([k]) => k.startsWith("tf-upload-tasks-"))
      .map(([, v]) => v.length),
  );
  expect(kept.length).toBe(1);
  expect(kept[0]).toBeLessThan(2000);

  const offsets = patchOffsets(page);
  await region.locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(page.getByText("1 file continues where it stopped.")).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached({ timeout: 60_000 });
  expect(offsets[0]).toBe(CHUNK);
  await expect(region).toHaveCount(0);

  const [file] = await children(page, target);
  expect(file.name).toBe("big-resume.bin");
  const content = await (await page.request.get(`/api/files/${file.id}/content`)).body();
  expect(sha(content)).toBe(sha(readFileSync(path)));
});

test("a file replaced since, with the same name, size and date, starts again instead of joining the old part", async ({ page }) => {
  await signIn(page);
  const target = await folder(page, "Recovery replaced");
  const path = diskFile("replaced.bin", 40 * MB, 2);
  const { mtime } = statSync(path);
  await page.goto(`/files/${target}`);
  await startAndHold(page, path);
  await reload(page);

  diskFile("replaced.bin", 40 * MB, 3);
  utimesSync(path, mtime, mtime);
  const offsets = patchOffsets(page);
  await page.getByRole("region", { name: "Interrupted uploads" }).locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(page.getByText("1 file changed since it was interrupted, so it starts again.")).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached({ timeout: 60_000 });
  expect(offsets[0]).toBe(0);
  const [file] = await children(page, target);
  const content = await (await page.request.get(`/api/files/${file.id}/content`)).body();
  expect(sha(content)).toBe(sha(readFileSync(path)));
});

test("an upload the server finished, whose answer was lost, isn't uploaded twice", async ({ page }) => {
  await signIn(page);
  const target = await folder(page, "Recovery finished");
  const path = diskFile("finished.bin", 2 * MB, 4);
  await page.goto(`/files/${target}`);
  // The server gets everything, but the page never hears back
  await page.route("**/api/uploads/*", async (route) => {
    if (route.request().method() === "PATCH") await route.fetch().catch(() => {});
    await route.abort();
  });
  await page.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await expect.poll(async () => (await children(page, target)).length, { timeout: 30_000 }).toBe(1);
  await page.unrouteAll({ behavior: "ignoreErrors" });
  await page.reload();

  const offsets = patchOffsets(page);
  await page.getByRole("region", { name: "Interrupted uploads" }).locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  expect(offsets).toEqual([]);
  expect((await children(page, target)).map((f) => f.name)).toEqual(["finished.bin"]);
});

test("another open tab's upload isn't offered, and a discarded one is gone from the server too", async ({ page, context }) => {
  await signIn(page);
  const target = await folder(page, "Recovery tabs");
  const path = diskFile("tabs.bin", 40 * MB, 5);
  await page.goto(`/files/${target}`);
  await startAndHold(page, path);

  const other = await context.newPage();
  await other.goto(`/files/${target}`);
  await expect(other.getByRole("heading", { name: /./ }).first()).toBeAttached();
  await other.waitForTimeout(1000);
  await expect(other.getByRole("region", { name: "Interrupted uploads" })).toHaveCount(0);

  // The first tab closes: after a while, the other one offers to continue its upload
  const url = await page.evaluate(() => {
    const key = Object.keys(localStorage).find((k) => k.startsWith("tus::"));
    return key ? JSON.parse(localStorage.getItem(key)!).uploadUrl : null;
  });
  await page.close();
  const region = other.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toContainText("tabs.bin", { timeout: 40_000 });

  await region.getByRole("button", { name: "Discard" }).click();
  // (the confirmation's button comes after the row's)
  await other.getByRole("button", { name: "Discard", exact: true }).last().click();
  await expect(region).toHaveCount(0);
  expect((await other.request.head(url, { headers: { "Tus-Resumable": "1.0.0" } })).status()).toBe(404);
  expect(await other.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("tf-upload-tasks-") || k.startsWith("tus::")))).toEqual([]);
});

test("a folder upload continues each file by its path, not by its name alone", async ({ page }) => {
  await signIn(page);
  const target = await folder(page, "Recovery folder");
  const root = join(dir, "Project");
  mkdirSync(join(root, "a"), { recursive: true });
  mkdirSync(join(root, "b"), { recursive: true });
  // Two files of one name in different folders: the small one arrives, the large one is interrupted
  const big = join(root, "a", "same.bin");
  writeFileSync(big, readFileSync(diskFile("big-folder.bin", 40 * MB, 6)));
  writeFileSync(join(root, "b", "same.bin"), "small");
  rmSync(join(dir, "big-folder.bin"));
  await page.goto(`/files/${target}`);
  let held = false;
  await page.route("**/api/uploads/*", async (route) => {
    const r = route.request();
    if (r.method() === "PATCH" && r.headers()["upload-offset"] !== "0") {
      held = true;
      return route.abort();
    }
    await route.continue();
  });
  await page.locator("input[webkitdirectory]").first().setInputFiles(root);
  await expect.poll(() => held, { timeout: 30_000 }).toBe(true);
  await reload(page);

  const region = page.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toContainText("Project");
  const offsets = patchOffsets(page);
  await region.locator("input[webkitdirectory]").setInputFiles(root);
  await expect(page.getByText("1 file continues where it stopped.")).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached({ timeout: 60_000 });
  expect(offsets[0]).toBe(CHUNK);
  const [project] = await children(page, target);
  const inside = await children(page, project.id);
  const [a] = await children(page, inside.find((f) => f.name === "a")!.id);
  const [b] = await children(page, inside.find((f) => f.name === "b")!.id);
  expect(sha(await (await page.request.get(`/api/files/${a.id}/content`)).body())).toBe(sha(readFileSync(big)));
  expect(await (await page.request.get(`/api/files/${b.id}/content`)).text()).toBe("small");
});

test("a share link's visitor gets their interrupted upload back on that link, and nowhere else", async ({ page, browser }) => {
  await signIn(page);
  const target = await folder(page, "Recovery link");
  const share = await (await page.request.post("/api/shares", { data: { node_id: target, allow_upload: true } })).json();
  const context = await browser.newContext();
  const visitor = await context.newPage();
  const path = diskFile("visitor.bin", 40 * MB, 7);
  await visitor.goto(`/share/${share.id}`);
  let held = false;
  await visitor.route("**/uploads/*", async (route) => {
    const r = route.request();
    if (r.method() === "PATCH" && r.headers()["upload-offset"] !== "0") {
      held = true;
      return route.abort();
    }
    await route.continue();
  });
  await visitor.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await expect.poll(() => held, { timeout: 30_000 }).toBe(true);
  await reload(visitor);
  const region = visitor.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toContainText("visitor.bin");
  // The link's token isn't in the records
  const leaks = await visitor.evaluate((token) => Object.entries(localStorage).some(([k, v]) => k.startsWith("tf-upload-tasks-") && (k + v).includes(token)), share.id);
  expect(leaks).toBe(false);
  // Another link doesn't offer it
  await visitor.goto("/share/nosuchlink");
  await expect(visitor.getByText("This share link doesn't exist or has expired")).toBeVisible();
  await expect(visitor.getByRole("region", { name: "Interrupted uploads" })).toHaveCount(0);

  await visitor.goto(`/share/${share.id}`);
  await region.locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(visitor.getByText("1 file continues where it stopped.")).toBeVisible();
  await expect(visitor.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached({ timeout: 60_000 });
  const [file] = await children(page, target);
  expect(sha(await (await page.request.get(`/api/files/${file.id}/content`)).body())).toBe(sha(readFileSync(path)));
  await context.close();
});
