// The PDF viewer: pages drawn with pdf.js, the panel of page pictures, going to a page, zoom and finding text
import { expect, test } from "@playwright/test";
import { makeFolder, signIn, uploadFile } from "./helpers";

/** A small PDF with one line of text on each page (pdf.js rebuilds the cross-reference table it leaves out) */
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

test("a PDF opens in the viewer: pages, page pictures, going to a page, zoom and finding text", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Reports");
  const id = await uploadFile(page, folder, "annual.pdf", pdfWithPages(["First page alpha", "Second page budget", "Third page budget"]));
  await page.goto(`/view/${id}`);

  const tools = page.getByRole("toolbar", { name: "PDF tools" });
  await expect(tools).toBeVisible();
  await expect(tools.getByText("of 3")).toBeVisible();
  // The text of the pages is there to select and copy
  const pages = page.getByRole("document", { name: "annual.pdf" });
  await expect(pages.getByText("First page alpha")).toBeVisible();

  // The panel shows a picture of each page; a click goes there
  const panel = page.getByRole("navigation", { name: "Pages" });
  await expect(panel.getByRole("button")).toHaveCount(3);
  await expect(panel.getByRole("button", { name: "Page 1" })).toHaveAttribute("aria-current", "page");
  await panel.getByRole("button", { name: "Page 2" }).click();
  await expect(panel.getByRole("button", { name: "Page 2" })).toHaveAttribute("aria-current", "page");
  await expect(tools.getByLabel("Page number")).toHaveValue("2");

  // A page number typed in
  await tools.getByLabel("Page number").fill("3");
  await tools.getByLabel("Page number").press("Enter");
  await expect(panel.getByRole("button", { name: "Page 3" })).toHaveAttribute("aria-current", "page");
  await expect(tools.getByRole("button", { name: "Next page" })).toBeDisabled();

  // Zoom a step at a time, or to a size picked
  await tools.getByLabel("Zoom", { exact: true }).selectOption("1");
  await expect(tools.getByLabel("Zoom", { exact: true })).toHaveValue("1");
  await tools.getByRole("button", { name: "Zoom in" }).click();
  await expect(tools.getByLabel("Zoom", { exact: true })).toHaveValue("1.25");

  // Find: from the page shown, matches counted and marked; Enter goes on to the next
  await tools.getByLabel("Page number").fill("1");
  await tools.getByLabel("Page number").press("Enter");
  await expect(panel.getByRole("button", { name: "Page 1" })).toHaveAttribute("aria-current", "page");
  const find = tools.getByRole("searchbox", { name: "Find in document" });
  await find.fill("budget");
  await expect(tools.getByRole("status")).toHaveText("Match 1 of 2");
  await expect(pages.locator(".textLayer .highlight").first()).toBeVisible();
  await find.press("Enter");
  await expect(tools.getByRole("status")).toHaveText("Match 2 of 2");
  await find.fill("nowhere-to-be-found");
  await expect(tools.getByRole("status")).toHaveText("Not found");

  // Downloading is offered to someone who may
  await expect(tools.getByRole("button", { name: "Print" })).toBeVisible();
  await expect(tools.getByRole("button", { name: "Download" })).toBeVisible();
});

test("a file that isn't a PDF inside says so rather than showing an empty page", async ({ page }) => {
  await signIn(page);
  const folder = await makeFolder(page, "Broken");
  const id = await uploadFile(page, folder, "broken.pdf", "this is not a PDF at all");
  await page.goto(`/view/${id}`);
  await expect(page.getByRole("alert")).toContainText("This file is damaged or isn't a PDF");
  await expect(page.getByRole("alert").getByRole("button", { name: "Download" })).toBeVisible();
});
