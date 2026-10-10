// Editing a Word document's text online and saving it: the file the server keeps has the new text, and what the editor
// didn't touch is kept as it was
import JSZip from "jszip";
import { expect, test, type Page } from "@playwright/test";
import { buildDocument, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
import { answer, makeFolder, signIn, uploadFile } from "./helpers";

const HEADING = '<w:p w14:paraId="10000001"><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Quarterly report</w:t></w:r></w:p>';
const BODY =
  HEADING +
  '<w:p><w:r><w:t xml:space="preserve">Sales grew by </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>25 percent</w:t></w:r><w:r><w:t xml:space="preserve">, see </w:t></w:r>' +
  '<w:hyperlink r:id="rId10"><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>the website</w:t></w:r></w:hyperlink></w:p>' +
  '<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First point</w:t></w:r></w:p>' +
  '<w:tbl><w:tblGrid><w:gridCol w:w="4500"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>North</w:t></w:r></w:p></w:tc></w:tr></w:tbl>' +
  '<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>End of the first section.</w:t></w:r></w:p>' +
  "<w:p><w:r><w:t>Last paragraph.</w:t></w:r></w:p>";

/** The server's answer to saving the document: it may wait its turn behind other tests' changes */
const saving = (page: Page, id: string) => answer(page, "PUT", `/api/files/${id}/content`);

async function openEditor(page: Page, id: string, name: string) {
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: "Edit document" }).click();
  const frame = page.frameLocator(`iframe[title="${name}"]`);
  const text = frame.getByRole("textbox", { name: "Document text" });
  await expect(text).toBeVisible();
  return { frame, text };
}

async function savedXml(page: Page, id: string) {
  const zip = await JSZip.loadAsync(await (await page.request.get(`/api/files/${id}/content`)).body());
  return { zip, xml: await zip.file("word/document.xml")!.async("string") };
}

test("text typed into a document is saved into the file, which keeps the rest", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word");
  const id = await uploadFile(page, dir, "report.docx", Buffer.from(await buildDocument(BODY)));
  const { frame, text } = await openEditor(page, id, "report.docx");

  // Add to the last paragraph, start a new one with Enter, and replace a bold word
  await frame.getByText("Last paragraph.").click();
  await text.press("End");
  await text.pressSequentially(" Added.");
  await text.press("Enter");
  await text.pressSequentially("A new paragraph");
  await frame.getByText("25 percent").dblclick({ position: { x: 40, y: 8 } });
  await text.pressSequentially("thirty");
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  const save = saving(page, id);
  await text.press("ControlOrMeta+s");
  expect((await save).ok()).toBe(true);
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeHidden();

  const { zip, xml } = await savedXml(page, id);
  expect(xml).toContain(">Last paragraph. Added.<");
  expect(xml).toMatch(/<w:p>(<w:pPr\/>)?<w:r><w:t>A new paragraph<\/w:t><\/w:r><\/w:p>/);
  // The word typed over the bold one is bold too
  expect(xml).toMatch(/<w:rPr><w:b\/><\/w:rPr><w:t>25 thirty<\/w:t>/);
  // What wasn't touched is as it was: the heading, the link, the list, the table and the section break
  expect(xml).toContain(HEADING);
  expect(xml).toContain('<w:hyperlink r:id="rId10">');
  expect(xml).toContain('<w:numId w:val="1"/>');
  expect(xml).toContain("<w:t>North</w:t>");
  expect(xml).toContain('<w:type w:val="continuous"/>');
  expect(await zip.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
});

test("a table can't be joined with the paragraph after it, and is kept", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word table");
  const body =
    '<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4500"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>After</w:t></w:r></w:p>';
  const id = await uploadFile(page, dir, "table.docx", Buffer.from(await buildDocument(body)));
  const { frame, text } = await openEditor(page, id, "table.docx");

  await frame.getByText("After").click();
  await text.press("Home");
  await text.press("Backspace");
  await text.pressSequentially("X");
  await expect(frame.getByText("XAfter")).toBeVisible();
  await expect(frame.getByText("Cell")).toBeVisible();

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  expect(xml).toMatch(/<\/w:tbl><w:p><w:r><w:t>XAfter<\/w:t><\/w:r><\/w:p>/);
});

test("unsaved edits are still there after leaving the document and coming back", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word draft");
  const id = await uploadFile(page, dir, "draft.docx", Buffer.from(await buildDocument("<w:p><w:r><w:t>Draft</w:t></w:r></w:p>")));
  const { frame, text } = await openEditor(page, id, "draft.docx");
  await frame.getByText("Draft").click();
  await text.press("End");
  await text.pressSequentially(" kept");
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // To the folder and back, without loading the page again
  await page.getByRole("button", { name: "Open file location" }).click();
  await page.waitForURL(`**/files/${dir}`);
  await page.goBack();
  const again = page.frameLocator('iframe[title="draft.docx"]');
  await expect(again.getByText("Draft kept")).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Stopping asks first; discarding leaves the file as it was
  await page.getByRole("button", { name: "Done editing" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Discard and exit" }).click();
  await expect(page.getByRole("button", { name: "Edit document" })).toBeVisible();
  const { xml } = await savedXml(page, id);
  expect(xml).toContain("<w:t>Draft</w:t>");
});

test("text typed into table cells is saved there, and Tab moves from cell to cell", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word cells");
  const cell = (props: string, text: string) => `<w:tc><w:tcPr><w:tcW w:w="4500"/>${props}</w:tcPr><w:p><w:r><w:t>${text}</w:t></w:r></w:p></w:tc>`;
  const header = cell('<w:shd w:val="clear" w:fill="FFFF00"/>', "Region") + cell("", "Sales");
  const body =
    "<w:p><w:r><w:t>Intro</w:t></w:r></w:p>" +
    '<w:tbl><w:tblPr><w:tblW w:w="9000"/></w:tblPr><w:tblGrid><w:gridCol w:w="4500"/><w:gridCol w:w="4500"/></w:tblGrid>' +
    `<w:tr>${header}</w:tr><w:tr>${cell("", "North")}${cell("", "120")}</w:tr></w:tbl><w:p/>`;
  const id = await uploadFile(page, dir, "cells.docx", Buffer.from(await buildDocument(body)));
  const { frame } = await openEditor(page, id, "cells.docx");

  await frame.getByText("North").click();
  await page.keyboard.press("End");
  await page.keyboard.type(" East");
  // Tab goes to the end of the next cell; Enter there starts a new paragraph in that cell
  await page.keyboard.press("Tab");
  await page.keyboard.type("0");
  await page.keyboard.press("Enter");
  await page.keyboard.type("units");
  // Shift+Tab goes back, Backspace at the start of a cell stays in it
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Home");
  await page.keyboard.press("Backspace");
  await expect(frame.getByText("North East")).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  const save = saving(page, id);
  await page.keyboard.press("ControlOrMeta+s");
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  expect(xml).toContain(`<w:tr>${header}</w:tr>`);
  expect(xml).toContain('<w:tc><w:tcPr><w:tcW w:w="4500"/></w:tcPr><w:p><w:r><w:t>North East</w:t></w:r></w:p></w:tc>');
  expect(xml).toMatch(/<w:tc><w:tcPr><w:tcW w:w="4500"\/><\/w:tcPr><w:p><w:r><w:t>1200<\/w:t><\/w:r><\/w:p><w:p><w:r><w:t>units<\/w:t><\/w:r><\/w:p><\/w:tc>/);
  expect(xml).toContain("<w:p><w:r><w:t>Intro</w:t></w:r></w:p><w:tbl>");
});

test("rows and columns are added and deleted from the menus, and Ctrl+Z right after undoes it", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word rows");
  const cell = (text: string) => `<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>${text}</w:t></w:r></w:p></w:tc>`;
  const body =
    "<w:p><w:r><w:t>Intro</w:t></w:r></w:p>" +
    '<w:tbl><w:tblPr><w:tblW w:w="6000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>' +
    `<w:tr>${cell("A")}${cell("B")}</w:tr><w:tr>${cell("C")}${cell("D")}</w:tr></w:tbl><w:p/>`;
  const id = await uploadFile(page, dir, "rows.docx", Buffer.from(await buildDocument(body)));
  const { frame } = await openEditor(page, id, "rows.docx");
  const menu = async (name: "Insert" | "Delete", item: string) => {
    await page.getByRole("button", { name, exact: true }).click();
    await page.getByRole("menuitem", { name: item }).click();
  };

  // The menus work on the table the cursor is in
  await expect(page.getByRole("button", { name: "Insert", exact: true })).toBeDisabled();
  await frame.getByText("C", { exact: true }).click();
  await menu("Insert", "Insert row below");
  await page.keyboard.type("E");
  await expect(frame.locator("tr")).toHaveCount(3);

  // Deleting a column, then Ctrl+Z right after, brings it back
  await frame.getByText("B", { exact: true }).click();
  await menu("Delete", "Delete column");
  await expect(frame.getByText("D", { exact: true })).toHaveCount(0);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame.getByText("D", { exact: true })).toBeVisible();

  await frame.getByText("A", { exact: true }).click();
  await menu("Insert", "Insert column right");
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  const rows = [...xml.matchAll(/<w:tr>(.*?)<\/w:tr>/g)].map((m) =>
    [...m[1].matchAll(/<w:tc>(.*?)<\/w:tc>/g)].map((c) => [...c[1].matchAll(/<w:t>([^<]*)<\/w:t>/g)].map((x) => x[1]).join("")),
  );
  expect(rows).toEqual([
    ["A", "", "B"],
    ["C", "", "D"],
    ["E", "", ""],
  ]);
  expect(xml).toContain('<w:tblW w:w="9000" w:type="dxa"/>');
  expect([...xml.matchAll(/<w:gridCol w:w="3000"\/>/g)]).toHaveLength(3);
});

test("cells selected together are merged, a cell is split from the dialog, and Ctrl+Z right after undoes it", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word merge");
  const cell = (text: string) => `<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>${text}</w:t></w:r></w:p></w:tc>`;
  const body =
    '<w:tbl><w:tblPr><w:tblW w:w="6000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>' +
    `<w:tr>${cell("A")}${cell("B")}</w:tr><w:tr>${cell("C")}${cell("D")}</w:tr></w:tbl><w:p/>`;
  const id = await uploadFile(page, dir, "merge.docx", Buffer.from(await buildDocument(body)));
  const { frame } = await openEditor(page, id, "merge.docx");
  const cellsMenu = async (item: string) => {
    await page.getByRole("button", { name: "Cells", exact: true }).click();
    await page.getByRole("menuitem", { name: item }).click();
  };
  const centre = async (text: string) => {
    const box = (await frame.getByText(text, { exact: true }).boundingBox())!;
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  };

  // Dragging from A to B selects both; merging joins them, and Ctrl+Z right after splits them again
  const a = await centre("A");
  const b = await centre("B");
  await page.mouse.move(a.x, a.y);
  await page.mouse.down();
  await page.mouse.move(b.x, b.y, { steps: 5 });
  await page.mouse.up();
  await expect(frame.locator("td[data-picked]")).toHaveCount(2);
  await cellsMenu("Merge cells");
  await expect(frame.locator("tr").first().locator("td")).toHaveCount(1);
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame.locator("tr").first().locator("td")).toHaveCount(2);

  // Shift+click selects from the cursor's cell: A and C merge down
  await frame.getByText("A", { exact: true }).click();
  await frame.getByText("C", { exact: true }).click({ modifiers: ["Shift"] });
  await expect(frame.locator("td[data-picked]")).toHaveCount(2);
  await cellsMenu("Merge cells");

  // D split into two columns
  await frame.getByText("D", { exact: true }).click();
  await cellsMenu("Split cell…");
  const dialog = page.getByRole("dialog", { name: "Split cell" });
  await dialog.getByLabel("Number of columns").fill("2");
  await dialog.getByLabel("Number of rows").fill("1");
  await dialog.getByRole("button", { name: "Split" }).click();
  await expect(frame.locator("tr").nth(1).locator("td")).toHaveCount(2);

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  const rows = [...xml.matchAll(/<w:tr>(.*?)<\/w:tr>/g)].map((m) =>
    [...m[1].matchAll(/<w:tc>(.*?)<\/w:tc>/g)].map((c) => ({
      text: [...c[1].matchAll(/<w:t>([^<]*)<\/w:t>/g)].map((x) => x[1]).join(""),
      span: Number(/<w:gridSpan w:val="(\d+)"\/>/.exec(c[1])?.[1] ?? 1),
      merge: /<w:vMerge w:val="restart"\/>/.test(c[1]) ? "restart" : /<w:vMerge\/>/.test(c[1]) ? "continue" : "-",
    })),
  );
  expect(rows).toEqual([
    [
      { text: "AC", span: 1, merge: "restart" },
      { text: "B", span: 2, merge: "-" },
    ],
    [
      { text: "", span: 1, merge: "continue" },
      { text: "D", span: 1, merge: "-" },
      { text: "", span: 1, merge: "-" },
    ],
  ]);
  expect([...xml.matchAll(/<w:gridCol w:w="(\d+)"\/>/g)].map((m) => m[1])).toEqual(["3000", "1500", "1500"]);
});
