// Shared by the end-to-end tests. Several workers run them at once against one server, and a test may run again on the
// same server (a retry), so:
// - names get a random part (`unique`), and a test looks its rows up inside a folder of its own;
// - after a change, a test waits for the server's answer (`answer`, `uploadFinished`), never a fixed time: a change
//   waits its turn behind the other tests' changes, and a large one (large-folder.spec.ts) holds the server for 10–40 s.
// scripts/check-e2e.mjs checks the specs for fixed waits and for folders and accounts made without these helpers.
import { randomBytes } from "node:crypto";
import { expect, type Page, type Response } from "@playwright/test";

export const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

/** The password of the accounts `makeUser` makes */
export const USER_PASSWORD = "a-long-test-password-1";

/** A random part for a name: copies of a test that run at once, or a test run again, don't share it */
export const unique = () => randomBytes(4).toString("hex");

/**
 * Signs in as the administrator. The sign-in page opens on a lock screen and moves the focus to the username field
 * once it shows: typing waits for that, so a busy machine doesn't send the password into the username field.
 */
export async function signIn(page: Page) {
  await page.goto("/");
  await page.getByRole("button", { name: "Click or press any key to sign in" }).click();
  const username = page.getByLabel("Username");
  await expect(username).toBeFocused();
  await username.fill("admin");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(/\/files/);
}

/** A new folder with a unique name in My files, or one with this name in `parent`: its id */
export async function makeFolder(page: Page, name: string, parent?: string): Promise<string> {
  const into = parent ?? (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: into, name: parent ? name : `${name} ${unique()}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

/** A new account named `<prefix>-<random part>`, made by the administrator signed in: its username */
export async function makeUser(page: Page, prefix: string, fields: Record<string, unknown> = {}): Promise<string> {
  const username = `${prefix}-${unique()}`;
  const res = await page.request.post("/api/admin/users", { data: { username, password: USER_PASSWORD, ...fields } });
  expect(res.ok()).toBe(true);
  return username;
}

/**
 * Signs the page in as a new account of its own, whose password is replaced first as the server asks. Its trash holds
 * only what the test puts there (the administrator's holds thousands of items from the other tests): its username.
 */
export async function signInAsNewUser(page: Page, prefix: string): Promise<string> {
  await signIn(page);
  const username = await makeUser(page, prefix);
  expect((await page.request.post("/api/auth/logout")).ok()).toBe(true);
  expect((await page.request.post("/api/auth/login", { data: { username, password: USER_PASSWORD } })).ok()).toBe(true);
  expect((await page.request.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  return username;
}

/**
 * Opens folder `id`, once the page has asked for what it holds: before that, it doesn't know the folder yet, and a file
 * given to its upload field would have nowhere to go
 */
export async function openFolder(page: Page, id: string) {
  const listed = listing(page, id);
  await page.goto(`/files/${id}`);
  await listed;
}

/** A file with this content, uploaded the way the page uploads (tus): its id */
export async function uploadFile(page: Page, parent: string, name: string, content: Buffer | string): Promise<string> {
  const bytes = Buffer.from(content);
  const b64 = (s: string) => Buffer.from(s).toString("base64");
  const created = await page.request.post("/api/uploads", {
    headers: { "Tus-Resumable": "1.0.0", "Upload-Length": String(bytes.length), "Upload-Metadata": `filename ${b64(name)},parentId ${b64(parent)}` },
  });
  expect(created.ok()).toBe(true);
  if (!bytes.length) return created.headers()["x-node-id"];
  const res = await page.request.patch(created.headers()["location"], {
    headers: { "Tus-Resumable": "1.0.0", "Upload-Offset": "0", "Content-Type": "application/offset+octet-stream" },
    data: bytes,
  });
  expect(res.ok()).toBe(true);
  return res.headers()["x-node-id"];
}

/**
 * The server's answer to the next request that finishes an upload made by the page, signed in or through a share link
 * (or the last of `count` of them): start waiting before the upload starts. The file is saved once it comes.
 */
export function uploadFinished(page: Page, count = 1) {
  let left = count;
  return page.waitForResponse((r) => /\/uploads(\/|$)/.test(new URL(r.url()).pathname) && r.ok() && r.headers()["x-node-id"] !== undefined && --left === 0);
}

/**
 * The server's answer to the page's next `method` request to `path` (the whole path, or a pattern for it), or the first
 * that comes once `after` has: start waiting before the action that sends it, then check the answer.
 */
export function answer(page: Page, method: string, path: string | RegExp, after?: Promise<unknown>): Promise<Response> {
  return answered(page, after, (r) => {
    const at = new URL(r.url()).pathname;
    return r.request().method() === method && (typeof path === "string" ? at === path : path.test(at));
  });
}

/**
 * The server's answer to the page's next listing of folder `id` (not the navigation pane's list of its folders), or the
 * first that comes once `after` has: the list loading again after a change, say
 */
export function listing(page: Page, id: string, after?: Promise<unknown>): Promise<Response> {
  return answered(page, after, (r) => {
    const url = new URL(r.url());
    return url.pathname === `/api/nodes/${id}/children` && !url.searchParams.has("folders_only");
  });
}

/** The first answer that `matches`, of those that come once `after` has (if given) */
function answered(page: Page, after: Promise<unknown> | undefined, matches: (r: Response) => boolean): Promise<Response> {
  let ready = !after;
  void after?.then(() => (ready = true));
  return page.waitForResponse((r) => ready && matches(r));
}
