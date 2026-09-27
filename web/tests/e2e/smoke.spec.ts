// Smoke test against the real server: sign in, upload, preview a Word document and a workbook, download.
import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import { buildDocx, buildWorkbook } from "../fixtures";

const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? "e2e-admin-password";

test("sign in, upload, preview and download", async ({ page }) => {
  const docx = Buffer.from(await buildDocx(["Hello from the smoke test", "Second paragraph"]));
  const xlsx = Buffer.from(
    await buildWorkbook(
      [
        { name: "Budget", rows: '<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>1234</v></c></row><row r="2"><c r="B2"><f>B1*2</f><v>2468</v></c></row>' },
        { name: "Notes", rows: '<row r="1"><c r="A1" t="s"><v>1</v></c></row>' },
      ],
      ["Rent", "Paid monthly"],
    ),
  );

  // Sign in (the page opens on a lock screen)
  await page.goto("/");
  await page.getByRole("button", { name: "Click or press any key to sign in" }).click();
  await page.getByLabel("Username").fill("admin");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(/\/files/);

  // Upload both files to "My files"
  await page.locator('input[type="file"][multiple]').setInputFiles([
    { name: "letter.docx", mimeType: "application/vnd.openxmlformats-officedocument.wordprocessingml.document", buffer: docx },
    { name: "budget.xlsx", mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", buffer: xlsx },
  ]);
  const row = (name: string) => page.locator("tbody tr").filter({ hasText: name });
  await expect(row("letter.docx")).toBeVisible();
  await expect(row("budget.xlsx")).toBeVisible();

  // Word: laid out in the sandboxed preview frame
  await row("letter.docx").dblclick();
  await page.waitForURL(/\/view\//);
  const frame = page.frameLocator("iframe").first();
  await expect(frame.getByText("Hello from the smoke test")).toBeVisible();
  await expect(frame.getByText("Second paragraph")).toBeVisible();

  // Excel: drawn on a canvas, with a tab for each sheet
  await page.goBack();
  await row("budget.xlsx").dblclick();
  await page.waitForURL(/\/view\//);
  await expect(page.getByRole("button", { name: "Budget", exact: true })).toBeVisible();
  await expect(page.locator("canvas").first()).toBeVisible();
  await page.getByRole("button", { name: "Notes", exact: true }).click();
  await expect(page.getByText(/Couldn't open/)).toHaveCount(0);

  // Download gives back the same bytes
  const downloading = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download", exact: true }).first().click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("budget.xlsx");
  expect(Buffer.compare(await readFile(await download.path()), xlsx)).toBe(0);
});
