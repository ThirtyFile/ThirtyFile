// Uploads interrupted by a reload continue once their files are chosen again, against the real server
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, openFolder, signIn, uploadFinished } from "./helpers";

const MB = 1024 * 1024;
/** tus-js-client sends 32 MB per request (uploads.ts) */
const CHUNK = 32 * MB;

// Each upload is a few changes on the server, and each may wait its turn behind a large change made by another test at
// the same time
test.describe.configure({ timeout: 120_000 });

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

async function children(page: Page, id: string): Promise<{ id: string; name: string; size: number }[]> {
  const res = await (await page.request.get(`/api/nodes/${id}/children?limit=100`)).json();
  return Array.isArray(res) ? res : res.items;
}

const sha = (b: Buffer) => createHash("sha256").update(b).digest("hex");

/**
 * Lets the uploads' first 32 MB through to the server (`url`: where the page sends uploads), and stops the rest: it
 * never gets through (the connection drops), until the page is reloaded. `held` comes once the server has the first
 * part.
 */
async function holdAfterFirstPart(page: Page, url = "**/api/uploads/*"): Promise<{ held: Promise<void> }> {
  let hold!: () => void;
  const held = new Promise<void>((resolve) => (hold = resolve));
  await page.route(url, async (route) => {
    const r = route.request();
    if (r.method() === "PATCH" && r.headers()["upload-offset"] !== "0") {
      hold();
      return route.abort();
    }
    await route.continue();
  });
  return { held };
}

/** Starts uploading a file and stops it after its first 32 MB reached the server */
async function startAndHold(page: Page, path: string) {
  const { held } = await holdAfterFirstPart(page);
  await page.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await held;
}

/** Reloads the page while the upload is stopped, then lets requests through again */
async function reload(page: Page) {
  await page.reload();
  await page.unrouteAll({ behavior: "ignoreErrors" });
}

/**
 * The server's answer when the page ends an upload (`url`: that upload's address, or any): the page waits for it, and
 * a server busy with other requests may take a while
 */
function uploadEnded(page: Page, url?: string) {
  return page.waitForResponse(
    (r) => r.request().method() === "DELETE" && (url ? new URL(r.url()).pathname === new URL(url, r.url()).pathname : new URL(r.url()).pathname.startsWith("/api/uploads/")),
  );
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
  const target = await makeFolder(page, "Recovery");
  const path = diskFile("big-resume.bin", 40 * MB, 1);
  await openFolder(page, target);
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
  const finished = uploadFinished(page);
  await region.locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(page.getByText("1 file continues where it stopped.")).toBeVisible();
  await finished;
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  expect(offsets[0]).toBe(CHUNK);
  await expect(region).toHaveCount(0);

  const [file] = await children(page, target);
  expect(file.name).toBe("big-resume.bin");
  const content = await (await page.request.get(`/api/files/${file.id}/content`)).body();
  expect(sha(content)).toBe(sha(readFileSync(path)));
});

test("a file replaced since, with the same name, size and date, starts again instead of joining the old part", async ({ page }) => {
  await signIn(page);
  const target = await makeFolder(page, "Recovery replaced");
  const path = diskFile("replaced.bin", 40 * MB, 2);
  const { mtime } = statSync(path);
  await openFolder(page, target);
  await startAndHold(page, path);
  await reload(page);

  const changed = readFileSync(path);
  changed[MB] ^= 1;
  changed[35 * MB] ^= 1;
  writeFileSync(path, changed);
  utimesSync(path, mtime, mtime);
  const offsets = patchOffsets(page);
  const finished = uploadFinished(page);
  // The page lets the server drop the part it can't continue, then says it starts again
  const ended = uploadEnded(page);
  await page.getByRole("region", { name: "Interrupted uploads" }).locator('input[type="file"][multiple]').setInputFiles(path);
  expect((await ended).status()).toBe(204);
  await expect(page.getByText("1 file changed since it was interrupted, so it starts again.")).toBeVisible();
  await finished;
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  expect(offsets[0]).toBe(0);
  const [file] = await children(page, target);
  const content = await (await page.request.get(`/api/files/${file.id}/content`)).body();
  expect(sha(content)).toBe(sha(readFileSync(path)));
});

test("a legacy sparse-sample record starts again instead of authorizing a resume", async ({ page }) => {
  await signIn(page);
  const target = await makeFolder(page, "Recovery legacy");
  const path = diskFile("legacy.bin", 40 * MB, 9);
  await openFolder(page, target);
  await startAndHold(page, path);
  await reload(page);
  // The page has looked at what is kept (it does so once, when it loads) before it is changed below
  await expect(page.getByRole("region", { name: "Interrupted uploads" })).toContainText("legacy.bin");
  // An earlier version kept a sparse sample, and a tus fingerprint made of the upload address and the file's details
  const oldUrl = await page.evaluate(() => {
    let url: string | null = null;
    for (const [key, value] of Object.entries(localStorage)) {
      if (key.startsWith("tf-upload-tasks-")) {
        const records = JSON.parse(value);
        for (const record of records) record.sample = "legacy-sparse-sample";
        localStorage.setItem(key, JSON.stringify(records));
      }
      if (key.startsWith("tus::")) {
        localStorage.setItem("tus::sd|/api/uploads|folder||keep|legacy.bin|41943040|1::1", value);
        localStorage.removeItem(key);
        url = JSON.parse(value).uploadUrl;
      }
    }
    return url;
  });
  expect(oldUrl).not.toBeNull();
  const ended = uploadEnded(page, oldUrl!);
  await page.reload();
  // Its upload address is no longer kept under the old key
  await expect.poll(() => page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("tus::sd|")))).toEqual([]);
  const offsets = patchOffsets(page);
  const finished = uploadFinished(page);
  await page.getByRole("region", { name: "Interrupted uploads" }).locator('input[type="file"][multiple]').setInputFiles(path);
  await finished;
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  expect(offsets[0]).toBe(0);
  expect((await ended).status()).toBe(204);
  expect((await page.request.head(oldUrl!, { headers: { "Tus-Resumable": "1.0.0" } })).status()).toBe(404);
  const [file] = await children(page, target);
  expect(sha(await (await page.request.get(`/api/files/${file.id}/content`)).body())).toBe(sha(readFileSync(path)));
});

test("an upload the server finished, whose answer was lost, isn't uploaded twice", async ({ page }) => {
  await signIn(page);
  const target = await makeFolder(page, "Recovery finished");
  const path = diskFile("finished.bin", 2 * MB, 4);
  await openFolder(page, target);
  // The server gets everything, but the page never hears back
  let saved!: () => void;
  const done = new Promise<void>((resolve) => (saved = resolve));
  await page.route("**/api/uploads/*", async (route) => {
    if (route.request().method() === "PATCH") {
      const res = await route.fetch().catch(() => null);
      if (res?.headers()["x-node-id"]) saved();
    }
    await route.abort();
  });
  await page.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await done;
  expect(await children(page, target)).toHaveLength(1);
  await page.unrouteAll({ behavior: "ignoreErrors" });
  await page.reload();

  const offsets = patchOffsets(page);
  // The page learns the server finished it, and ends the upload before saying it is complete
  const ended = uploadEnded(page);
  await page.getByRole("region", { name: "Interrupted uploads" }).locator('input[type="file"][multiple]').setInputFiles(path);
  expect((await ended).status()).toBe(204);
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  expect(offsets).toEqual([]);
  expect((await children(page, target)).map((f) => f.name)).toEqual(["finished.bin"]);
});

test("another open tab's upload isn't offered, and a discarded one is gone from the server too", async ({ page, context }) => {
  // Time flows as usual in both tabs, until the test moves it on (uploadRecovery.ts: BEAT_MS, STALE_MS)
  await context.clock.install();
  await signIn(page);
  const target = await makeFolder(page, "Recovery tabs");
  const path = diskFile("tabs.bin", 40 * MB, 5);
  await openFolder(page, target);
  await startAndHold(page, path);

  const other = await context.newPage();
  await other.goto(`/files/${target}`);
  // Signed in and showing the folder: the interrupted uploads were worked out, and the first tab's record is there
  await expect(other.getByText("No files here yet")).toBeVisible();
  await expect.poll(() => other.evaluate(() => Object.entries(localStorage).some(([k, v]) => k.startsWith("tf-upload-tasks-") && v.includes("tabs.bin")))).toBe(true);
  // A beat later the first tab is still there, so the other one doesn't offer its upload
  await context.clock.runFor(5_000);
  const region = other.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toHaveCount(0);

  // The first tab closes: once its record went stale, the other one offers to continue its upload
  const url = await page.evaluate(() => {
    const key = Object.keys(localStorage).find((k) => k.startsWith("tus::"));
    return key ? JSON.parse(localStorage.getItem(key)!).uploadUrl : null;
  });
  expect(url).not.toBeNull();
  await page.close();
  await context.clock.fastForward(25_000);
  await expect(region).toContainText("tabs.bin");

  await region.getByRole("button", { name: "Discard" }).click();
  // The server lets go of the upload first, then the row goes: it may be busy with other requests for a while
  const deleted = uploadEnded(other, url);
  await other.getByRole("dialog").getByRole("button", { name: "Discard", exact: true }).click();
  expect((await deleted).status()).toBe(204);
  await expect(region).toHaveCount(0);
  expect((await other.request.head(url, { headers: { "Tus-Resumable": "1.0.0" } })).status()).toBe(404);
  expect(await other.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("tf-upload-tasks-") || k.startsWith("tus::")))).toEqual([]);
});

test("a folder upload continues each file by its path, not by its name alone", async ({ page }) => {
  await signIn(page);
  const target = await makeFolder(page, "Recovery folder");
  const root = join(dir, "Project");
  mkdirSync(join(root, "a"), { recursive: true });
  mkdirSync(join(root, "b"), { recursive: true });
  // Two files of one name in different folders: the small one arrives, the large one is interrupted
  const big = join(root, "a", "same.bin");
  writeFileSync(big, readFileSync(diskFile("big-folder.bin", 40 * MB, 6)));
  writeFileSync(join(root, "b", "same.bin"), "small");
  rmSync(join(dir, "big-folder.bin"));
  await openFolder(page, target);
  const { held } = await holdAfterFirstPart(page);
  await page.locator("input[webkitdirectory]").first().setInputFiles(root);
  await held;
  await reload(page);

  const region = page.getByRole("region", { name: "Interrupted uploads" });
  await expect(region).toContainText("Project");
  const offsets = patchOffsets(page);
  const finished = uploadFinished(page);
  await region.locator("input[webkitdirectory]").setInputFiles(root);
  await expect(page.getByText("1 file continues where it stopped.")).toBeVisible();
  await finished;
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
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
  const target = await makeFolder(page, "Recovery link");
  const share = await (await page.request.post("/api/shares", { data: { node_id: target, allow_upload: true } })).json();
  const context = await browser.newContext();
  const visitor = await context.newPage();
  const path = diskFile("visitor.bin", 40 * MB, 7);
  await visitor.goto(`/share/${share.id}`);
  // The link's folder has loaded: its upload field knows where files go
  await expect(visitor.getByText("This folder is empty")).toBeVisible();
  const { held } = await holdAfterFirstPart(visitor, "**/uploads/*");
  await visitor.locator('input[type="file"][multiple]').first().setInputFiles(path);
  await held;
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
  const finished = uploadFinished(visitor);
  await region.locator('input[type="file"][multiple]').setInputFiles(path);
  await expect(visitor.getByText("1 file continues where it stopped.")).toBeVisible();
  await finished;
  await expect(visitor.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  const [file] = await children(page, target);
  expect(sha(await (await page.request.get(`/api/files/${file.id}/content`)).body())).toBe(sha(readFileSync(path)));
  await context.close();
});
