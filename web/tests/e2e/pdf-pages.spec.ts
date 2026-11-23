// Page operations on PDFs: organize in a page grid (as a new file, and as a new version), extract by range, split, and combine
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

const row = (page: Page, name: string) => page.locator("[data-node-id]").filter({ hasText: name });

/** A PDF with one line of text on each page */
function pdfWithPages(lines: string[]): string {
  const n = lines.length;
  const kids = lines.map((_, i) => `${3 + i * 2} 0 R`).join(" ");
  const font = 3 + n * 2;
  const objects = [`1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj`, `2 0 obj\n<< /Type /Pages /Kids [${kids}] /Count ${n} >>\nendobj`];
  lines.forEach((line, i) => {
    const content = `BT /F1 24 Tf 72 700 Td (${line}) Tj ET`;
    objects.push(
      `${3 + i * 2} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 ${font} 0 R >> >> /Contents ${4 + i * 2} 0 R >>\nendobj`,
      `${4 + i * 2} 0 obj\n<< /Length ${content.length} >>\nstream\n${content}\nendstream\nendobj`,
    );
  });
  objects.push(`${font} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj`);
  return `%PDF-1.4\n${objects.join("\n")}\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n`;
}

/** The text of each page of a PDF the server made, read from the file itself */
async function pageTexts(page: Page, id: string): Promise<string[]> {
  const res = await page.request.get(`/api/files/${id}/content`);
  expect(res.ok()).toBe(true);
  const text = (await res.body()).toString("latin1");
  expect(text.startsWith("%PDF-")).toBe(true);
  return [...text.matchAll(/\(([^)]*)\) Tj/g)].map((m) => m[1]);
}

/** The items of a folder, by name */
async function listing(page: Page, folder: string): Promise<Record<string, string>> {
  const items: { id: string; name: string }[] = await (await page.request.get(`/api/nodes/${folder}/children`)).json();
  return Object.fromEntries(items.map((n) => [n.name, n.id]));
}

async function openFolder(page: Page, folder: string, first: string) {
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${folder}`);
  await expect(row(page, first)).toBeVisible();
}

test("pages are organized in a grid and saved as a new file, then as a new version that can be restored", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Organize");
  const id = await uploadFile(page, folder, "scan.pdf", pdfWithPages(["Alpha", "Bravo", "Charlie"]));
  await openFolder(page, folder, "scan.pdf");

  await row(page, "scan.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Organize pages…" }).click();
  const dialog = page.getByRole("dialog", { name: "Organize pages" });
  const grid = dialog.getByRole("list", { name: "Pages" });
  await expect(grid.getByRole("listitem")).toHaveCount(3);

  // Delete the second page, turn the first, and move the last one before it: then undo the move
  await dialog.getByRole("button", { name: "Delete page 2" }).click();
  await expect(grid.getByRole("listitem")).toHaveCount(2);
  await dialog.getByRole("button", { name: "Turn page 1 right" }).click();
  await dialog.getByRole("button", { name: "Move page 2 earlier" }).click();
  await expect(grid.getByRole("listitem").first()).toContainText("1 (was 3)");
  await dialog.getByRole("button", { name: "Undo" }).click();
  await expect(grid.getByRole("listitem").first()).not.toContainText("was");

  // As a new file next to the original
  await dialog.getByLabel("File name").fill("Two pages.pdf");
  await dialog.getByRole("button", { name: "Save as new file" }).click();
  await expect(page.getByText('Created "Two pages.pdf"')).toBeVisible();
  const items = await listing(page, folder);
  expect(await pageTexts(page, items["Two pages.pdf"])).toEqual(["Alpha", "Charlie"]);
  expect(await pageTexts(page, id)).toEqual(["Alpha", "Bravo", "Charlie"]);

  // As a new version of the same file, in the other order
  await row(page, "scan.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Organize pages…" }).click();
  await expect(grid.getByRole("listitem")).toHaveCount(3);
  await dialog.getByRole("button", { name: "Move page 3 earlier" }).click();
  await dialog.getByRole("button", { name: "Save as new version" }).click();
  await expect(page.getByText('Saved a new version of "scan.pdf"')).toBeVisible();
  expect(await pageTexts(page, id)).toEqual(["Alpha", "Charlie", "Bravo"]);
  const versions: { id: string }[] = await (await page.request.get(`/api/files/${id}/versions`)).json();
  expect(versions).toHaveLength(1);
  expect((await page.request.post(`/api/files/${id}/versions/${versions[0].id}/restore`)).ok()).toBe(true);
  expect(await pageTexts(page, id)).toEqual(["Alpha", "Bravo", "Charlie"]);
});

test("pages are extracted by range and a PDF is split into files", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Extract");
  await uploadFile(page, folder, "book.pdf", pdfWithPages(["One", "Two", "Three", "Four"]));
  await openFolder(page, folder, "book.pdf");

  await row(page, "book.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Extract pages…" }).click();
  const dialog = page.getByRole("dialog", { name: "Extract pages" });
  await dialog.getByLabel("Pages to take").fill("4, 1-2");
  await dialog.getByLabel("File name").fill("Picked.pdf");
  await dialog.getByRole("button", { name: "Extract", exact: true }).click();
  await expect(page.getByText('Created "Picked.pdf"')).toBeVisible();
  expect(await pageTexts(page, (await listing(page, folder))["Picked.pdf"])).toEqual(["Four", "One", "Two"]);

  // A range past the last page is refused with a message
  await row(page, "book.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Extract pages…" }).click();
  await dialog.getByLabel("Pages to take").fill("1-9");
  await dialog.getByRole("button", { name: "Extract", exact: true }).click();
  await expect(page.getByText("Couldn't extract the pages")).toBeVisible();
  await expect(page.getByText(/goes past the last/)).toBeVisible();

  // Split into a file per page
  await row(page, "book.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Split PDF…" }).click();
  await page.getByRole("dialog", { name: "Split PDF" }).getByRole("button", { name: "Split", exact: true }).click();
  await expect(page.getByText("Split into 4 files")).toBeVisible();
  const items = await listing(page, folder);
  expect(await pageTexts(page, items["book - 3.pdf"])).toEqual(["Three"]);
});

test("PDFs are combined in the order chosen", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Combine");
  await uploadFile(page, folder, "a.pdf", pdfWithPages(["A1", "A2"]));
  await uploadFile(page, folder, "b.pdf", pdfWithPages(["B1"]));
  await openFolder(page, folder, "a.pdf");

  await row(page, "a.pdf").click();
  await row(page, "b.pdf").click({ modifiers: ["Control"] });
  await row(page, "b.pdf").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Combine PDFs…" }).click();
  const dialog = page.getByRole("dialog", { name: "Combine PDFs" });
  // b.pdf first
  await dialog.getByRole("button", { name: "Move b.pdf up" }).click();
  await dialog.getByLabel("File name").fill("Both.pdf");
  await dialog.getByRole("button", { name: "Combine", exact: true }).click();
  await expect(page.getByText('Created "Both.pdf"')).toBeVisible();
  expect(await pageTexts(page, (await listing(page, folder))["Both.pdf"])).toEqual(["B1", "A1", "A2"]);
});
