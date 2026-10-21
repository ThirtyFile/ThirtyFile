import { test, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import os from "node:os";
import { buildDocx, buildWorkbook, buildDocument } from "../fixtures";
import { buildPresentation } from "../office-fixtures";
import { answer, signIn, makeFolder, uploadFile } from "./helpers";
import { office, officeCall, officeFile, readyOffice, openOffice } from "./office-sdk.helpers";

test.setTimeout(240_000);
const screenshots = path.join(os.tmpdir(), "thirtyfile-office-qa");

test("ThirtyFile hosts all packaged editors, persists their files and keeps the full native chrome", async ({ page }) => {
  const failures: string[] = [];
  const workers: any[] = [];
  await page.addInitScript(() => {
    const NativeWorker = window.Worker;
    const records: any[] = [];
    Object.assign(window, { thirtyFileWorkerEvidence: records });
    window.Worker = class extends NativeWorker {
      record: any;
      constructor(url: string | URL, options?: WorkerOptions) {
        super(url, options);
        this.record = { url: String(url), terminated: false };
        records.push(this.record);
        this.addEventListener("message", (event) => {
          if (event.data.type === "ready") Object.assign(this.record, { runtime: event.data.runtime, context: event.data.context });
        });
      }
      terminate() {
        this.record.terminated = true;
        super.terminate();
      }
    };
    WebAssembly.instantiate = async () => {
      throw new Error("Main-thread WASM is forbidden");
    };
    WebAssembly.instantiateStreaming = async () => {
      throw new Error("Main-thread WASM is forbidden");
    };
  });
  await signIn(page);
  page.on("pageerror", (error) => failures.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") failures.push(message.text());
  });
  const folder = await makeFolder(page, "Office SDK");
  const word = await uploadFile(page, folder, "document.docx", Buffer.from(await buildDocx(["ThirtyFile document"])));
  const sheet = await uploadFile(page, folder, "workbook.xlsx", Buffer.from(await buildWorkbook([{ name: "Data", rows: "" }])));
  const ppt = await uploadFile(page, folder, "presentation.pptx", Buffer.from(await buildPresentation()));
  for (const [id, kind] of [
    [word, "word"],
    [sheet, "sheets"],
    [ppt, "slides"],
  ] as const) {
    await openOffice(page, id, kind);
    const commands = await officeCall(page, "listCommands");
    expect(commands.result.length).toBeGreaterThan(100);
    const tabs = office(page).locator('[data-office-chrome] [role="tab"][data-id^="tab:"]');
    expect(await tabs.count()).toBeGreaterThanOrEqual(9);
    const initial = await officeCall(page, "getState");
    const agent = office(page).getByRole("textbox", { name: "Office editor text input" });
    if (kind === "word") {
      await agent.press("Control+End");
      await agent.pressSequentially(" ThirtyFile SDK ");
      await agent.press("Control+b");
      await agent.pressSequentially("bold");
      await agent.press("Control+b");
      await expect.poll(async () => (await officeCall(page, "getState")).engine.content.text).toContain("ThirtyFile SDK bold");
    } else if (kind === "sheets") {
      await agent.press("Control+Home");
      await agent.pressSequentially("12");
      await agent.press("Enter");
      await agent.pressSequentially("30");
      await agent.press("Enter");
      await agent.press("Control+Home");
      await agent.press("ArrowRight");
      await agent.pressSequentially("=SUM(A1:A2)");
      await agent.press("Enter");
      await expect.poll(async () => (await officeCall(page, "getState")).engine.content.viewport.values[0][1]).toBe("42");
    } else {
      await officeCall(page, "execute", "slides.insertTextBox", { text: "ThirtyFile PPT", rect: [80, 100, 350, 100] });
      await agent.press("Delete");
      await agent.press("Control+z");
      await agent.press("F5");
      await expect.poll(async () => (await officeCall(page, "inspectUI")).result.showing !== null).toBe(true);
      await agent.press("Escape");
    }
    const saved = answer(page, "PUT", `/api/files/${id}/content`);
    await agent.press("Control+s");
    expect((await saved).ok()).toBe(true);
    await expect(
      office(page)
        .getByRole("status")
        .filter({ hasText: /^Saved$/ }),
    ).toBeVisible();
    expect((await officeCall(page, "getState")).revision).not.toBe(initial.revision);
    const zip = await officeFile(page, id);
    expect(Object.keys(zip.files).some((name) => name === (kind === "word" ? "word/document.xml" : kind === "sheets" ? "xl/workbook.xml" : "ppt/presentation.xml"))).toBe(true);
    await mkdir(screenshots, { recursive: true });
    await page.screenshot({ path: path.join(screenshots, `${kind}.png`) });
    const versions = await (await page.request.get(`/api/files/${id}/versions`)).json();
    expect(versions.length).toBeGreaterThan(0);
    await page.reload();
    await page.getByRole("button", { name: kind === "word" ? "Edit document" : kind === "sheets" ? "Edit workbook" : "Edit presentation", exact: true }).click();
    await readyOffice(page);
    const content = (await officeCall(page, "getState")).engine.content;
    if (kind === "word") expect(content.text).toContain("ThirtyFile SDK bold");
    if (kind === "sheets") expect(content.viewport.values[0][1]).toBe("42");
    if (kind === "slides") expect(JSON.stringify(content)).toContain("ThirtyFile PPT");
    workers.push(...(await page.evaluate(() => (window as any).thirtyFileWorkerEvidence.filter((record: any) => record.url.includes("/office-assets/")))));
  }
  expect(workers.filter((worker) => worker.runtime).length).toBeGreaterThanOrEqual(3);
  expect(workers.every((worker) => (worker.runtime ? worker.runtime === "dedicated-worker" && !worker.context.hasWindow && !worker.context.hasDocument : worker.terminated))).toBe(true);
  expect(failures).toEqual([]);
});

test("Office drafts survive file navigation and conflict with a newer server version", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office drafts");
  const id = await uploadFile(page, folder, "draft.docx", Buffer.from(await buildDocx(["Draft"])));
  await openOffice(page, id, "word");
  await officeCall(page, "execute", "word.insertText", { text: " retained" });
  const stateId = (await officeCall(page, "getState")).engine.stateId;
  await page.getByRole("button", { name: "Open file location", exact: true }).click();
  await page.waitForURL(`**/files/${folder}`);
  await page.goBack();
  await readyOffice(page);
  expect((await officeCall(page, "getState")).engine.stateId).toBe(stateId);
  expect((await officeCall(page, "getState")).dirty).toBe(true);
  const changed = await page.request.put(`/api/files/${id}/content`, { data: Buffer.from(await buildDocx(["Other client"])) });
  expect(changed.ok()).toBe(true);
  await office(page).getByRole("button", { name: "Save", exact: true }).click();
  await expect(office(page).getByRole("alert")).toContainText("Someone else changed");
  expect((await officeCall(page, "getState")).dirty).toBe(true);
  await office(page).getByRole("button", { name: "Reload latest version", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Reload latest version", exact: true }).click();
  await expect.poll(async () => (await officeCall(page, "getState")).engine.content.text).toContain("Other client");
});

test("a lost Office save response retries the same committed request without another version", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office retry");
  const id = await uploadFile(page, folder, "retry.docx", Buffer.from(await buildDocx(["Retry"])));
  await openOffice(page, id, "word");
  await officeCall(page, "execute", "word.insertText", { text: " committed" });
  await page.evaluate(() => {
    const native = window.fetch.bind(window);
    let dropped = false;
    const requests: unknown[] = [];
    Object.assign(window, { officeSaveRequests: requests });
    window.fetch = async (input, init) => {
      const request = new Request(new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url, location.href), init);
      if (request.method !== "PUT" || !request.url.endsWith("/content")) return native(input, init);
      requests.push({ key: request.headers.get("Idempotency-Key"), bytes: [...new Uint8Array(await request.clone().arrayBuffer())] });
      const response = await native(request);
      if (response.ok && !dropped) {
        dropped = true;
        await response.arrayBuffer();
        throw new TypeError("Simulated acknowledgement loss after commit");
      }
      return response;
    };
  });
  await office(page).getByRole("button", { name: "Save", exact: true }).click();
  await expect(office(page).getByRole("button", { name: "Retry save", exact: true })).toBeVisible();
  const count = (await (await page.request.get(`/api/files/${id}/versions`)).json()).length;
  await office(page).getByRole("button", { name: "Retry save", exact: true }).click();
  await expect(
    office(page)
      .getByRole("status")
      .filter({ hasText: /^Saved$/ }),
  ).toBeVisible();
  const requests = await page.evaluate(() => (window as any).officeSaveRequests);
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
  expect((await (await page.request.get(`/api/files/${id}/versions`)).json()).length).toBe(count);
});

test("public Office shares enforce readonly and disabled export for every format", async ({ page, context }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office shares");
  const files = [
    ["shared.docx", Buffer.from(await buildDocx(["Shared Word"]))],
    ["shared.xlsx", Buffer.from(await buildWorkbook([{ name: "Shared", rows: '<row r="1"><c r="A1"><v>7</v></c></row>' }]))],
    ["shared.pptx", Buffer.from(await buildPresentation())],
  ] as const;
  const ids: string[] = [];
  for (const [name, bytes] of files) ids.push(await uploadFile(page, folder, name, bytes));
  const response = await page.request.post("/api/shares", { data: { node_id: folder, allow_download: false } });
  expect(response.ok()).toBe(true);
  const token = (await response.json()).id;
  await context.clearCookies();
  for (const [name] of files) {
    await page.goto(`/share/${token}`);
    await page.getByText(name, { exact: true }).dblclick();
    await readyOffice(page);
    expect((await officeCall(page, "getState")).mode).toBe("readonly");
    await expect(officeCall(page, "execute", "undo", {})).rejects.toThrow("read-only");
    await expect(officeCall(page, "export")).rejects.toThrow("Export is disabled");
    await expect(page.getByRole("button", { name: "Download", exact: true })).toHaveCount(0);
  }
  expect((await page.request.put(`/api/files/${ids[0]}/content`, { data: "refused" })).status()).toBe(401);
});

test("closing an Office draft discards it and terminates its dedicated Worker", async ({ page }) => {
  await page.addInitScript(() => {
    const NativeWorker = window.Worker;
    const records: { url: string; terminated: boolean }[] = [];
    Object.assign(window, { officeWorkerLifetimes: records });
    window.Worker = class extends NativeWorker {
      record: { url: string; terminated: boolean };
      constructor(url: string | URL, options?: WorkerOptions) {
        super(url, options);
        this.record = { url: String(url), terminated: false };
        records.push(this.record);
      }
      terminate() {
        this.record.terminated = true;
        super.terminate();
      }
    };
  });
  await signIn(page);
  expect((await page.request.put("/api/auth/style", { data: { style: "windows" } })).ok()).toBe(true);
  const folder = await makeFolder(page, "Office lifetime");
  const id = await uploadFile(page, folder, "retire.docx", Buffer.from(await buildDocx(["Original"])));
  await openOffice(page, id, "word");
  await officeCall(page, "execute", "word.insertText", { text: "draft" });
  await page.getByRole("tab", { name: "retire.docx", exact: true }).locator('span[title="Unsaved changes"]').click();
  await page.getByRole("dialog").getByRole("button", { name: "Discard and close", exact: true }).click();
  await expect(page.locator("[data-office-session]")).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => (window as any).officeWorkerLifetimes.filter((item: any) => item.url.includes("office-assets")).every((item: any) => item.terminated)))
    .toBe(true);
  expect(await (await officeFile(page, id)).file("word/document.xml")!.async("string")).not.toContain("draft");
});

test("saving a detached Office tab drains typing into its own file", async ({ page }) => {
  await signIn(page);
  expect((await page.request.put("/api/auth/style", { data: { style: "windows" } })).ok()).toBe(true);
  const folder = await makeFolder(page, "Office detached save");
  const id = await uploadFile(page, folder, "background.docx", Buffer.from(await buildDocx(["Original"])));
  await openOffice(page, id, "word");
  const session = await office(page)
    .locator("[data-office-session]")
    .evaluateHandle((element) => (element as any).officeSession);
  const agent = office(page).getByRole("textbox", { name: "Office editor text input" });
  await agent.press("Control+End");
  await agent.pressSequentially(" complete detached text");
  await page.getByRole("button", { name: "New tab", exact: true }).click();
  await page.waitForURL(/\/files$/);
  await session.evaluate((session) => session.save());
  const xml = await (await officeFile(page, id)).file("word/document.xml")!.async("string");
  expect(xml).toContain("complete detached text");
  await page.getByRole("tab", { name: "background.docx", exact: true }).click();
  await readyOffice(page);
  expect((await officeCall(page, "getState")).dirty).toBe(false);
});

test("readonly Office previews scroll and switch sheets and slides without editing", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office view navigation");
  const word = await uploadFile(
    page,
    folder,
    "pages.docx",
    Buffer.from(await buildDocument("<w:p><w:r><w:t>First page</w:t></w:r></w:p><w:p><w:pPr><w:pageBreakBefore/></w:pPr><w:r><w:t>Second page</w:t></w:r></w:p>")),
  );
  const sheet = await uploadFile(
    page,
    folder,
    "sheets.xlsx",
    Buffer.from(
      await buildWorkbook([
        { name: "First", rows: '<row r="1"><c r="A1"><v>11</v></c></row>' },
        { name: "Second", rows: '<row r="1"><c r="A1"><v>22</v></c></row>' },
      ]),
    ),
  );
  const slides = await uploadFile(page, folder, "slides.pptx", Buffer.from(await buildPresentation(2)));
  await page.goto(`/view/${word}`);
  await readyOffice(page);
  const before = (await officeCall(page, "inspectUI")).result.pages[0][1];
  const canvas = await office(page).locator("canvas").boundingBox();
  await page.mouse.move(canvas!.x + canvas!.width / 2, canvas!.y + canvas!.height / 2);
  await page.mouse.wheel(0, 600);
  await expect.poll(async () => (await officeCall(page, "inspectUI")).result.pages[0][1]).toBeLessThan(before);
  for (const [id, label] of [
    [sheet, "Preview worksheet"],
    [slides, "Preview slide"],
  ]) {
    await page.goto(`/view/${id}`);
    await readyOffice(page);
    await page.getByLabel(label, { exact: true }).selectOption("1");
    await expect.poll(async () => (await officeCall(page, "getState")).engine.content.navigation.index).toBe(1);
    expect((await officeCall(page, "getState")).dirty).toBe(false);
    await expect(officeCall(page, "navigate", { command: "undo" })).rejects.toThrow();
    await expect(officeCall(page, "execute", "undo", {})).rejects.toThrow("read-only");
    if (id === sheet) expect((await officeCall(page, "getState")).engine.content.viewport.values[0][0]).toBe("22");
  }
});

test("PPT toolbar actions preserve the original slideshow shortcut", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "PPT toolbar shortcut");
  const id = await uploadFile(page, folder, "shortcut.pptx", Buffer.from(await buildPresentation()));
  await openOffice(page, id, "slides");
  await office(page).getByRole("button", { name: "New Slide", exact: true }).click();
  const agent = office(page).getByRole("textbox", { name: "Office editor text input", exact: true });
  await agent.press("F5");
  await expect
    .poll(async () => {
      const showing = (await officeCall(page, "inspectUI")).result.showing;
      return showing !== null && typeof showing === "object";
    })
    .toBe(true);
  await agent.press("Escape");
  await expect.poll(async () => (await officeCall(page, "inspectUI")).result.showing).toBeNull();
});

test("Office controls wait for the actual file before accepting input", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office pending load");
  const files = [
    ["word", "pending.docx", await buildDocx(["Actual document"])],
    ["sheets", "pending.xlsx", await buildWorkbook([{ name: "Actual", rows: '<row r="1"><c r="A1"><v>123</v></c></row>' }])],
    ["slides", "pending.pptx", await buildPresentation(2)],
  ] as const;
  for (const [kind, name, bytes] of files) {
    const id = await uploadFile(page, folder, name, Buffer.from(bytes));
    await page.goto(`/view/${id}`);
    await readyOffice(page);
    for (const action of [kind === "word" ? "Edit document" : kind === "sheets" ? "Edit workbook" : "Edit presentation", "Reload latest version"]) {
      let release!: () => void;
      let received!: () => void;
      const gate = new Promise<void>((resolve) => (release = resolve));
      const requested = new Promise<void>((resolve) => (received = resolve));
      const url = `**/api/files/${id}/content`;
      await page.route(url, async (route) => {
        received();
        await gate;
        await route.continue();
      });
      try {
        await page.getByRole("button", { name: action, exact: true }).click();
        await requested;
        await expect(office(page)).toHaveAttribute("data-office-ready", "false");
        await expect(office(page).getByRole("textbox", { name: "Office editor text input", exact: true })).toBeHidden();
      } finally {
        release();
      }
      await readyOffice(page);
      await page.unroute(url);
      await expect(office(page).getByRole("textbox", { name: "Office editor text input", exact: true })).toBeVisible();
    }
    const state = await officeCall(page, "getState");
    if (kind === "word") expect(state.engine.content.text).toContain("Actual document");
    if (kind === "sheets") expect(state.engine.content.viewport.values[0][0]).toBe("123");
    if (kind === "slides") expect(state.engine.content.navigation.items).toHaveLength(2);
  }
});

test("packaged editors omit upstream branding and promotion and expose legal notices", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Office community links");
  const files = [
    ["word", "community.docx", await buildDocx(["Hosted document"])],
    ["sheets", "community.xlsx", await buildWorkbook([{ name: "Hosted", rows: "" }])],
    ["slides", "community.pptx", await buildPresentation()],
  ] as const;
  const noPromotion = () =>
    expect
      .poll(() =>
        office(page)
          .locator("[data-office-dom]")
          .evaluate((element: any) =>
            /WordCraft|GridCraft|DeckCraft|ArtCraft|[Dd]iscord|getartcraft\.com|github\.com\/storytold|Source on GitHub|Source code:/.test(JSON.stringify(element.model)),
          ),
      )
      .toBe(false);
  for (const [kind, name, bytes] of files) {
    const id = await uploadFile(page, folder, name, Buffer.from(bytes));
    await openOffice(page, id, kind);
    if (kind === "word") await officeCall(page, "execute", "ui.language", { value: "en" });
    await noPromotion();
    if (kind === "word") {
      await office(page).locator('[data-office-chrome] [data-id="tab:Help"]').click();
      await expect(office(page).getByRole("button", { name: "Commands", exact: true })).toBeVisible();
      await noPromotion();
    }
    await officeCall(page, "execute", kind === "slides" ? "app.about" : "ui.dialog", kind === "slides" ? {} : { name: "about" });
    const dialog = office(page).getByRole("dialog");
    await expect(dialog).toBeVisible();
    await expect
      .poll(() =>
        office(page)
          .locator("[data-office-dom]")
          .evaluate((element: any) => JSON.stringify(element.model).includes("About this editor")),
      )
      .toBe(true);
    await noPromotion();
    await dialog.getByRole("button", { name: "LICENSE-MIT", exact: true }).and(dialog.locator("button:not(.drag)")).click();
    const notice = dialog.getByRole("textbox", { name: "LICENSE-MIT contents", exact: true });
    await expect(notice).toHaveJSProperty("readOnly", true);
    await expect.poll(() => notice.inputValue()).toContain("Permission is hereby granted");
    await page.screenshot({ path: path.join(os.tmpdir(), `thirtyfile-office-${kind}-licenses.png`) });
    await dialog.getByRole("button", { name: "LICENSE-MIT", exact: true }).and(dialog.locator("button:not(.drag)")).click();
    await noPromotion();
    await dialog
      .getByRole("button", { name: /^Close(?: window)?$/ })
      .last()
      .click();
    if (kind === "word") {
      for (const pane of ["general", "save", "agents"]) {
        await officeCall(page, "execute", "ui.backstage", { value: true, page: "options", pane });
        await noPromotion();
      }
    }
    if (kind === "slides") {
      for (const pageName of ["home", "new", "account", "saveAs"]) {
        await officeCall(page, "execute", "app.backstage", { page: pageName });
        await noPromotion();
        await expect
          .poll(() =>
            office(page)
              .locator("[data-office-dom]")
              .evaluate((element: any) => JSON.stringify(element.model).includes("Office JS SDK 0.8.0")),
          )
          .toBe(false);
      }
      await expect(officeCall(page, "execute", "app.links", {})).rejects.toThrow("Community links are not available");
    }
  }
});
