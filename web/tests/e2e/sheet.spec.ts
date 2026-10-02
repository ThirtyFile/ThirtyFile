// Editing a workbook online and saving it: the file the server keeps has the new values, and what the editor didn't
// touch is kept
import JSZip from "jszip";
import { expect, test } from "@playwright/test";
import { buildWorkbook, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
import { makeFolder, signIn, uploadFile } from "./helpers";

test("a value typed into a workbook is saved into the file, which keeps the rest", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet");
  const xlsx = Buffer.from(await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1234</v></c></row>' }], ["Rent"]));
  const id = await uploadFile(page, dir, "budget.xlsx", xlsx);
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: "Edit workbook" }).click();

  // Go to C1 with the name box, type a value and a formula below it, and save with Ctrl+S
  const nameBox = page.getByLabel("Name box");
  await expect(nameBox).toBeVisible();
  await nameBox.fill("C1");
  await nameBox.press("Enter");
  await page.keyboard.type("42");
  await page.keyboard.press("Enter");
  await page.keyboard.type("=B1+C1");
  await page.keyboard.press("Enter");
  await page.keyboard.press("ControlOrMeta+s");
  await expect(page.getByText("Saved (2 cells)")).toBeVisible();

  const saved = await JSZip.loadAsync(await (await page.request.get(`/api/files/${id}/content`)).body());
  const sheet = await saved.file("xl/worksheets/sheet1.xml")!.async("string");
  expect(sheet).toMatch(/<c r="C1"[^>]*><v>42<\/v><\/c>/);
  expect(sheet).toMatch(/<c r="C2"[^>]*><f>B1\+C1<\/f>(<v>1276<\/v>)?<\/c>/);
  expect(sheet).toMatch(/<c r="B1"[^>]*><v>1234<\/v><\/c>/);
  // A part the editor doesn't know comes back as it was
  expect(await saved.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
});

test("restoring an earlier version while the workbook is open for editing shows it there, and the next save goes through", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet restore");
  const xlsx = Buffer.from(await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="B1"><v>1234</v></c></row>' }], []));
  const id = await uploadFile(page, dir, "restore.xlsx", xlsx);
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: "Edit workbook" }).click();
  const nameBox = page.getByLabel("Name box");
  const go = async (cell: string) => {
    await nameBox.fill(cell);
    await nameBox.press("Enter");
  };
  await go("C1");
  await page.keyboard.type("42");
  await page.keyboard.press("Enter");
  await page.keyboard.press("ControlOrMeta+s");
  await expect(page.getByText("Saved (1 cell)")).toBeVisible();

  // The version from before the save, restored from the details pane
  await page.getByRole("button", { name: "Details pane" }).click();
  await page.getByRole("region", { name: "Versions" }).getByRole("button", { name: "Restore" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
  await expect(page.getByText("Version restored")).toBeVisible();
  // The workbook is opened again, at its first cell
  await expect(nameBox).toHaveValue("A1");
  await go("C1");
  await expect(page.getByLabel("Formula bar")).toHaveValue("");

  // Saved over the restored content, not refused as someone else's change
  await go("D1");
  await page.keyboard.type("7");
  await page.keyboard.press("Enter");
  await page.keyboard.press("ControlOrMeta+s");
  const sheet = async () => {
    const saved = await JSZip.loadAsync(await (await page.request.get(`/api/files/${id}/content`)).body());
    return saved.file("xl/worksheets/sheet1.xml")!.async("string");
  };
  await expect.poll(sheet).toMatch(/<c r="D1"[^>]*><v>7<\/v><\/c>/);
  expect(await sheet()).not.toMatch(/<c r="C1"/);
  await expect(page.getByText(/someone else|changed since/i)).toHaveCount(0);
});
