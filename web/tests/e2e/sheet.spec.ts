// Workbook acceptance through the packaged SDK and ThirtyFile's content/version APIs.
import { expect, test, type Page } from "@playwright/test";
import { buildWorkbook, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
import { answer, makeFolder, signIn, uploadFile } from "./helpers";
import { office, officeCall, officeFile, openOffice, sheetGo, sheetRead } from "./office-sdk.helpers";

const agent = (page: Page) => office(page).getByRole("textbox", { name: "Office editor text input" });
async function save(page: Page, id: string) {
  const response = answer(page, "PUT", `/api/files/${id}/content`);
  await agent(page).press("Control+s");
  expect((await response).ok()).toBe(true);
  await expect.poll(async () => (await officeCall(page, "getState")).dirty).toBe(false);
  return officeFile(page, id);
}

test("a value typed into a workbook is saved into the file, which keeps the rest", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet");
  const xlsx = Buffer.from(await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1234</v></c></row>' }], ["Rent"]));
  const id = await uploadFile(page, dir, "budget.xlsx", xlsx);
  await openOffice(page, id, "sheets");
  await sheetGo(page, "C1");
  await agent(page).pressSequentially("42");
  await agent(page).press("Enter");
  await agent(page).pressSequentially("=B1+C1");
  await agent(page).press("Enter");
  const saved = await save(page, id);
  const sheet = await saved.file("xl/worksheets/sheet1.xml")!.async("string");
  expect(sheet).toMatch(/<c r="C1"[^>]*><v>42<\/v><\/c>/);
  expect(sheet).toMatch(/<c r="C2"[^>]*><f>B1\+C1<\/f>(<v>1276<\/v>)?<\/c>/);
  expect(sheet).toMatch(/<c r="B1"[^>]*><v>1234<\/v><\/c>/);
  expect(await saved.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
});

test("restoring an earlier version while the workbook is open for editing shows it there, and the next save goes through", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet restore");
  const id = await uploadFile(page, dir, "restore.xlsx", Buffer.from(await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="B1"><v>1234</v></c></row>' }])));
  await openOffice(page, id, "sheets");
  await sheetGo(page, "C1");
  await agent(page).pressSequentially("42");
  await agent(page).press("Enter");
  await save(page, id);
  await page.getByRole("button", { name: "Details pane" }).click();
  await page.getByRole("region", { name: "Versions" }).getByRole("button", { name: "Restore" }).click();
  const restored = answer(page, "POST", new RegExp(`^/api/files/${id}/versions/[^/]+/restore$`));
  await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
  expect((await restored).ok()).toBe(true);
  await expect(page.getByText("Version restored")).toBeVisible();
  await expect.poll(() => sheetRead(page, "C1")).toEqual([[""]]);
  await sheetGo(page, "D1");
  await agent(page).pressSequentially("7");
  await agent(page).press("Enter");
  const saved = await save(page, id);
  const xml = await saved.file("xl/worksheets/sheet1.xml")!.async("string");
  expect(xml).toMatch(/<c r="D1"[^>]*><v>7<\/v><\/c>/);
  expect(xml).not.toMatch(/<c r="C1"/);
  await expect(page.getByText(/someone else|changed since/i)).toHaveCount(0);
});

test("an edit after a cut cancels the cut, so a paste then doesn't clear the cells", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet cut");
  const id = await uploadFile(page, dir, "cut.xlsx", Buffer.from(await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="B1"><v>1234</v></c></row>' }])));
  await openOffice(page, id, "sheets");
  await sheetGo(page, "B1");
  await officeCall(page, "execute", "edit.cut", {});
  await sheetGo(page, "C3");
  await agent(page).pressSequentially("5");
  await agent(page).press("Enter");
  await sheetGo(page, "D1");
  await officeCall(page, "execute", "edit.paste", {});
  const saved = await save(page, id);
  const xml = await saved.file("xl/worksheets/sheet1.xml")!.async("string");
  for (const [cell, value] of [
    ["B1", 1234],
    ["D1", 1234],
    ["C3", 5],
  ])
    expect(xml).toMatch(new RegExp(`<c r="${cell}"[^>]*><v>${value}</v></c>`));
});
