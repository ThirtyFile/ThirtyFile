// Editing, typed Control panel paths and transfer states against a real isolated server.
import { expect, test, type Page } from "@playwright/test";
import { buildWorkbook } from "../fixtures";
import { answer, makeFolder, makeUser, openFolder, signIn, signInAsNewUser, uploadFile, uploadFinished, USER_PASSWORD } from "./helpers";

test.describe.configure({ mode: "parallel" });

function held() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

async function admin(page: Page, style: "windows" | "mac") {
  await signIn(page);
  const username = await makeUser(page, "paths-admin", { role: "admin" });
  expect((await page.request.post("/api/auth/logout")).ok()).toBe(true);
  expect((await page.request.post("/api/auth/login", { data: { username, password: USER_PASSWORD } })).ok()).toBe(true);
  expect((await page.request.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  expect((await page.request.put("/api/auth/style", { data: { style } })).ok()).toBe(true);
  await page.goto("/files");
  await expect(page.getByRole("heading", { name: "My files", exact: true, level: 1 })).toBeVisible();
}

test("a delayed text save stays with its own file when switching two cached tabs", async ({ page }) => {
  await signInAsNewUser(page, "save-tabs");
  const dir = await makeFolder(page, "Save tabs");
  const a = await uploadFile(page, dir, "a.txt", "original A");
  const b = await uploadFile(page, dir, "b.txt", "original B");
  await page.goto(`/view/${b}`);
  await expect(page.locator(".cm-content")).toHaveText("original B");
  await page.getByRole("button", { name: "New tab" }).click();
  await page.waitForURL(/\/files$/);
  await page.getByRole("navigation", { name: "File path" }).click();
  const path = page.getByRole("textbox", { name: "Full path" });
  await path.fill(`${new URL(page.url()).origin}/view/${a}`);
  await path.press("Enter");
  await expect(page.locator(".cm-content")).toHaveText("original A");
  const waiting = held();
  const arrived = held();
  await page.route(`**/api/files/${b}/content`, async (route) => {
    if (route.request().method() === "PUT") {
      arrived.release();
      await waiting.promise;
    }
    await route.continue();
  });
  await page.getByRole("tab", { name: "b.txt" }).click();
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type("edited B");
  const saved = answer(page, "PUT", `/api/files/${b}/content`);
  await page.keyboard.press("ControlOrMeta+s");
  await arrived.promise;
  await page.getByRole("tab", { name: "a.txt" }).click();
  await expect(page.locator(".cm-content")).toHaveText("original A");
  waiting.release();
  expect((await saved).ok()).toBe(true);
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  await page.getByRole("tab", { name: "b.txt" }).click();
  await expect(page.locator(".cm-content")).toHaveText("edited B");
  await expect(page.getByRole("tab", { name: "b.txt" }).locator("span[title]")).toHaveAttribute("title", "Close tab");
  expect(await (await page.request.get(`/api/files/${a}/content`)).text()).toBe("original A");
  expect(await (await page.request.get(`/api/files/${b}/content`)).text()).toBe("edited B");
});

test("formula bar and name box keep candidate selection inside an input method", async ({ page }) => {
  await signInAsNewUser(page, "ime-sheet");
  const dir = await makeFolder(page, "IME sheet");
  const xlsx = Buffer.from(await buildWorkbook([{ name: "Sheet1", rows: '<row r="1"><c r="A1"><v>1</v></c></row>' }]));
  const id = await uploadFile(page, dir, "ime.xlsx", xlsx);
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: "Edit workbook" }).click();
  const bar = page.getByLabel("Formula bar");
  const box = page.getByLabel("Name box");
  await bar.fill("ㄓ");
  await bar.dispatchEvent("compositionstart", { data: "ㄓ" });
  for (const key of ["Enter", "Escape", "Tab"]) {
    await bar.dispatchEvent("keydown", { key, isComposing: true });
    await expect(bar).toBeFocused();
    await expect(bar).toHaveValue("ㄓ");
    await expect(box).toHaveValue("A1");
  }
  await bar.dispatchEvent("keydown", { key: "Enter", keyCode: 229 });
  await expect(bar).toBeFocused();
  await bar.fill("中文");
  await bar.dispatchEvent("compositionend", { data: "中文" });
  await bar.press("Enter");
  await expect(box).toHaveValue("A2");
  await box.fill("A1");
  await box.dispatchEvent("keydown", { key: "Enter", isComposing: true });
  await expect(box).toBeFocused();
  await expect(bar).toHaveValue("");
  await box.press("Enter");
  await expect(bar).toHaveValue("中文");
});

test("cutting cells from another workbook preserves unrelated destination cells", async ({ page }) => {
  await signInAsNewUser(page, "cut-books");
  const dir = await makeFolder(page, "Cut workbooks");
  const a = await uploadFile(page, dir, "source.xlsx", Buffer.from(await buildWorkbook([{ name: "Source", rows: '<row r="1"><c r="A1" t="inlineStr"><is><t>from A</t></is></c></row>' }])));
  const b = await uploadFile(
    page,
    dir,
    "destination.xlsx",
    Buffer.from(await buildWorkbook([{ name: "Destination", rows: '<row r="1"><c r="A1" t="inlineStr"><is><t>keep B</t></is></c></row>' }])),
  );
  await page.goto(`/view/${a}`);
  await page.getByRole("button", { name: "Edit workbook" }).click();
  await expect(page.getByLabel("Formula bar")).toHaveValue("from A");
  const text = await page.getByLabel("Cell contents").evaluate((el) => {
    const data = new DataTransfer();
    el.dispatchEvent(new ClipboardEvent("cut", { bubbles: true, cancelable: true, clipboardData: data }));
    return data.getData("text/plain");
  });
  expect(text).toBe("from A");
  await page.getByRole("button", { name: "New tab" }).click();
  await page.waitForURL(/\/files$/);
  await page.getByRole("navigation", { name: "File path" }).click();
  const path = page.getByRole("textbox", { name: "Full path" });
  await path.fill(`${new URL(page.url()).origin}/view/${b}`);
  await path.press("Enter");
  await page.getByRole("button", { name: "Edit workbook" }).click();
  const box = page.getByLabel("Name box");
  await box.fill("B1");
  await box.press("Enter");
  await page.getByLabel("Cell contents").evaluate((el, text) => {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    el.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: data }));
  }, text);
  await expect(page.getByLabel("Formula bar")).toHaveValue("from A");
  await box.fill("A1");
  await box.press("Enter");
  await expect(page.getByLabel("Formula bar")).toHaveValue("keep B");
});

test("a paste past the last column is rejected without altering the next row", async ({ page }) => {
  await signInAsNewUser(page, "paste-edge");
  const dir = await makeFolder(page, "Paste bounds");
  const id = await uploadFile(
    page,
    dir,
    "edge.xlsx",
    Buffer.from(await buildWorkbook([{ name: "Edge", rows: '<row r="2"><c r="A2" t="inlineStr"><is><t>keep next row</t></is></c></row>' }])),
  );
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: "Edit workbook" }).click();
  const box = page.getByLabel("Name box");
  await box.fill("XFD1");
  await box.press("Enter");
  await page.getByLabel("Cell contents").evaluate((el) => {
    const data = new DataTransfer();
    data.setData("text/plain", "first\tsecond");
    el.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: data }));
  });
  await expect(page.getByText("The pasted cells would extend beyond the worksheet. Select another cell and try again.")).toBeVisible();
  await box.fill("A2");
  await box.press("Enter");
  await expect(page.getByLabel("Formula bar")).toHaveValue("keep next row");
});

test("Windows address bar opens copied and canonical Control panel paths", async ({ page }) => {
  await admin(page, "windows");
  await page.getByRole("navigation", { name: "File path" }).click();
  let box = page.getByRole("textbox", { name: "Full path" });
  await box.fill("/admin/storage");
  await box.press("Enter");
  await page.waitForURL(/\/admin\/storage$/);
  await page.getByRole("navigation", { name: "File path" }).click();
  box = page.getByRole("textbox", { name: "Full path" });
  await expect(box).toHaveValue("/Control panel/Storage locations");
  const copied = await box.inputValue();
  await box.press("Escape");
  await page.goto("/files");
  await page.getByRole("navigation", { name: "File path" }).click();
  await box.fill(copied);
  await box.press("Enter");
  await page.waitForURL(/\/admin\/storage$/);
});

test("Mac Go to folder opens a Control panel display path", async ({ page }) => {
  await admin(page, "mac");
  await page.keyboard.press("ControlOrMeta+Shift+KeyG");
  const dialog = page.getByRole("dialog", { name: "Go to folder" });
  const box = dialog.getByRole("textbox", { name: "Full path" });
  await box.fill("/Control panel/Storage usage");
  await box.press("Enter");
  await page.waitForURL(/\/admin\/usage$/);
  await expect(dialog).toHaveCount(0);
});

test("typed Control panel paths do not bypass administrator access", async ({ page }) => {
  await signInAsNewUser(page, "paths-user");
  await page.goto("/files");
  await page.getByRole("navigation", { name: "File path" }).click();
  const box = page.getByRole("textbox", { name: "Full path" });
  await box.fill("/admin/users");
  await box.press("Enter");
  await expect(box).toHaveCount(0);
  await page.waitForURL(/\/files/);
  expect((await page.request.get("/api/admin/users")).status()).toBe(403);
  await expect(page.getByRole("option", { name: /^Users/ })).toHaveCount(0);
});

test("upload preparation and final server confirmation both show cancellable waiting states", async ({ page }) => {
  await page.addInitScript(() => {
    const original = XMLHttpRequest;
    const tracked = window as Window & { uploadRequests?: XMLHttpRequest[] };
    tracked.uploadRequests = [];
    window.XMLHttpRequest = class extends original {
      constructor() {
        super();
        tracked.uploadRequests!.push(this);
      }
    };
  });
  await signInAsNewUser(page, "upload-states");
  const dir = await makeFolder(page, "Upload states");
  await openFolder(page, dir);
  const create = held();
  const finish = held();
  const stored = held();
  await page.route("**/api/uploads", async (route) => {
    await create.promise;
    await route.continue();
  });
  await page.route("**/api/uploads/*", async (route) => {
    if (route.request().method() !== "PATCH") return route.continue();
    const response = await route.fetch();
    stored.release();
    await finish.promise;
    await route.fulfill({ response });
  });
  const completed = uploadFinished(page);
  await page
    .locator('input[type="file"][multiple]')
    .first()
    .setInputFiles({ name: "waiting.txt", mimeType: "text/plain", buffer: Buffer.from("transfer payload") });
  await expect(page.getByText("Preparing upload…", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
  await expect(page.getByRole("progressbar", { name: "Upload progress" })).not.toHaveAttribute("aria-valuenow");
  create.release();
  await stored.promise;
  // Playwright's response interception suppresses native upload progress until fulfillment. Deliver the browser
  // progress event while the real server's successful response is held, so finishing is tested independently.
  await page.evaluate(() => {
    const tracked = window as Window & { uploadRequests?: XMLHttpRequest[] };
    const request = tracked.uploadRequests!.findLast((request) => request.upload.onprogress !== null)!;
    request.upload.dispatchEvent(new ProgressEvent("progress", { lengthComputable: true, loaded: 16, total: 16 }));
  });
  await expect(page.getByText("Finishing upload…", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("progressbar", { name: "Upload progress" })).not.toHaveAttribute("aria-valuenow");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
  await expect(page.getByText("Finishing upload…", { exact: true }).last()).toBeVisible();
  finish.release();
  expect((await completed).ok()).toBe(true);
  await expect(page.getByText("1 upload complete", { exact: true }).first()).toBeVisible();
  await expect(page.locator("[data-node-id]").filter({ hasText: "waiting.txt" })).toBeVisible();
});

test("a pending archive link can be canceled before a download starts", async ({ page }) => {
  await signInAsNewUser(page, "download-states");
  const dir = await makeFolder(page, "Download states");
  await uploadFile(page, dir, "one.txt", "one");
  await uploadFile(page, dir, "two.txt", "two");
  await openFolder(page, dir);
  const prepare = held();
  const arrived = held();
  await page.route("**/api/download", async (route) => {
    arrived.release();
    await prepare.promise;
    await route.continue().catch(() => {});
  });
  await page.locator("[data-node-id]").filter({ hasText: "one.txt" }).click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.locator("[data-node-id]").filter({ hasText: "one.txt" }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Download (ZIP)", exact: true }).click();
  await arrived.promise;
  await expect(page.getByText("Preparing download…", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("progressbar", { name: "Download progress" })).not.toHaveAttribute("aria-valuenow");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByText("Preparing download…", { exact: true }).last()).toBeVisible();
  await page.getByRole("button", { name: "Cancel download" }).click();
  await expect(page.getByText("Download canceled", { exact: true }).first()).toBeVisible();
  prepare.release();
  await expect(page.getByRole("button", { name: "Download again" })).toBeVisible();
});
