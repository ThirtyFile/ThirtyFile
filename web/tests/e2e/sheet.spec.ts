// Editing a workbook online and saving it: the file the server keeps has the new values, and what the editor didn't
// touch is kept
import JSZip from "jszip";
import { expect, test } from "@playwright/test";
import { buildWorkbook, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
import { makeFolder, signIn, uploadFile } from "./helpers";

test("a value typed into a workbook is saved into the file, which keeps the rest", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Sheet");
  const xlsx = Buffer.from(
    await buildWorkbook([{ name: "Budget", rows: '<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1234</v></c></row>' }], ["Rent"]),
  );
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
