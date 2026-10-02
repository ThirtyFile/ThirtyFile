// Shared by the end-to-end tests.
import { expect, type Page } from "@playwright/test";

export const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

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
  const res = await page.request.post("/api/folders", { data: { parent_id: into, name: parent ? name : `${name} ${Date.now().toString(36)}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
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
 * The server's answer to the request that finishes an upload made by the page: start waiting before the upload starts.
 * The file is saved once it comes, and a server busy with large changes from other tests may take a while to answer.
 */
export function uploadFinished(page: Page) {
  return page.waitForResponse((r) => new URL(r.url()).pathname.startsWith("/api/uploads") && r.ok() && r.headers()["x-node-id"] !== undefined);
}
