// Editing a Word document's text online and saving it: the file the server keeps has the new text, and what the editor
// didn't touch is kept as it was
import JSZip from "jszip";
import { expect, test, type Page } from "@playwright/test";
import { buildDocument, buildDocx, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
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
  // The view opens again with the new row, the cursor in it
  await expect(frame.locator("tr")).toHaveCount(3);
  await expect(frame.locator("td[data-cell]:focus")).toHaveCount(1);
  await page.keyboard.type("E");

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

test("borders, shading and column widths are changed, and cells selected together stay selected", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word look");
  const cell = (text: string) => `<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>${text}</w:t></w:r></w:p></w:tc>`;
  const body =
    '<w:tbl><w:tblPr><w:tblW w:w="6000" w:type="dxa"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>' +
    `<w:tr>${cell("A")}${cell("B")}</w:tr><w:tr>${cell("C")}${cell("D")}</w:tr></w:tbl><w:p/>`;
  const id = await uploadFile(page, dir, "look.docx", Buffer.from(await buildDocument(body)));
  const { frame } = await openEditor(page, id, "look.docx");

  // All borders round the cell under the cursor
  await frame.getByText("A", { exact: true }).click();
  await page.getByRole("button", { name: "Borders" }).click();
  await page.getByRole("menuitem", { name: "All borders" }).click();
  await expect(frame.locator("td").first()).toBeVisible();

  // Shading for A and B together; they stay selected for the next change
  await frame.getByText("A", { exact: true }).click();
  await frame.getByText("B", { exact: true }).click({ modifiers: ["Shift"] });
  await page.getByRole("button", { name: "Shading" }).click();
  // The palette has this color twice (in its first and last rows)
  await page.getByRole("menuitem", { name: "#FFC000" }).first().click();
  await expect(frame.locator("td[data-picked]")).toHaveCount(2);

  // A's right border dragged 60 pixels to the right, once the menu has closed
  await expect(page.getByRole("menu")).toBeHidden();
  const a = (await frame.locator("td").first().boundingBox())!;
  await page.mouse.move(a.x + a.width - 1, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(a.x + a.width + 59, a.y + a.height / 2, { steps: 6 });
  await page.mouse.up();
  await expect.poll(async () => (await frame.locator("td").first().boundingBox())!.width).toBeGreaterThan(a.width + 40);

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  const cells = [...xml.matchAll(/<w:tc>(.*?)<\/w:tc>/g)].map((m) => m[1]);
  expect(cells[0]).toContain('<w:tcBorders><w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/><w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/>');
  expect(cells[1]).toContain('<w:left w:val="single" w:sz="4" w:space="0" w:color="auto"/>');
  expect(cells[2]).toContain('<w:top w:val="single" w:sz="4" w:space="0" w:color="auto"/>');
  for (const c of cells.slice(0, 2)) expect(c).toContain('<w:shd w:val="clear" w:color="auto" w:fill="FFC000"/>');
  expect(cells[2]).not.toContain("<w:shd");
  const widths = [...xml.matchAll(/<w:gridCol w:w="(\d+)"\/>/g)].map((m) => Number(m[1]));
  expect(widths[0] + widths[1]).toBe(6000);
  expect(widths[0]).toBeGreaterThan(3600);
  expect(xml).toContain(`<w:tcW w:w="${widths[0]}" w:type="dxa"/>`);
});

test("text is formatted from the toolbar and with shortcuts, and the file gets just those changes", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word format");
  const body = HEADING + "<w:p><w:r><w:t>Hello world</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p>";
  const id = await uploadFile(page, dir, "format.docx", Buffer.from(await buildDocument(body)));
  const { frame, text } = await openEditor(page, id, "format.docx");
  const bold = page.getByRole("button", { name: "Bold" });

  // Bold from the toolbar, which then shows it pressed; italic with Ctrl+I
  await frame.getByText("Hello world").dblclick({ position: { x: 60, y: 8 } });
  await bold.click();
  await expect(bold).toHaveAttribute("aria-pressed", "true");
  // A second command on the same words, from a menu that takes the focus away
  await page.getByRole("button", { name: "Highlight" }).click();
  await page.getByRole("menuitem", { name: "#00FF00" }).click();
  await frame
    .getByText("Hello", { exact: false })
    .first()
    .dblclick({ position: { x: 8, y: 8 } });
  await text.press("ControlOrMeta+i");

  // Color and highlight for "Second", then its size (the view opens again on the changed document)
  await frame.getByText("Second").dblclick();
  await page.getByRole("button", { name: "Font color" }).click();
  await page.getByRole("menu", { name: "Font color" }).getByRole("menuitem", { name: "#FF0000" }).first().click();
  await page.getByRole("button", { name: "Highlight" }).click();
  // The font color's menu may still be closing: the color is picked in the highlight's
  await page.getByRole("menu", { name: "Highlight" }).getByRole("menuitem", { name: "#FFFF00" }).click();
  await page.getByLabel("Font size").selectOption("14");
  await expect(page.getByLabel("Font size")).toHaveValue("14");

  // The first paragraph centered, the last made a heading
  await frame.getByText("world").click();
  await page.getByRole("button", { name: "Center" }).click();
  await expect(page.getByRole("button", { name: "Center" })).toHaveAttribute("aria-pressed", "true");
  await frame.getByText("Second").click();
  await page.getByLabel("Paragraph style").selectOption({ label: "Heading 1" });
  await expect(page.getByLabel("Paragraph style")).toHaveValue("Heading1");

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  expect(xml).toContain(HEADING);
  const paragraphs = [...xml.matchAll(/<w:p>(.*?)<\/w:p>/g)].map((m) => m[1]);
  const hello = paragraphs.find((p) => p.includes("Hello"))!;
  expect(hello).toContain('<w:pPr><w:jc w:val="center"/></w:pPr>');
  // A double-click selects the word and the space after it, as Windows does
  expect(hello).toMatch(/<w:r><w:rPr><w:i\/><w:iCs\/><\/w:rPr><w:t xml:space="preserve">Hello <\/w:t><\/w:r>/);
  expect(hello).toMatch(/<w:r><w:rPr><w:b\/><w:bCs\/><w:highlight w:val="green"\/><\/w:rPr><w:t>world<\/w:t><\/w:r>/);
  const second = paragraphs.find((p) => p.includes("Second"))!;
  expect(second).toContain('<w:pStyle w:val="Heading1"/>');
  expect(second).toMatch(/<w:rPr><w:color w:val="FF0000"\/><w:sz w:val="28"\/><w:szCs w:val="28"\/><w:highlight w:val="yellow"\/><\/w:rPr><w:t>Second<\/w:t>/);
});

test("bulleted and numbered lists are made, and a document without lists gets its numbering part", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word lists");
  // A bare document: no numbering part, not even relationships
  const id = await uploadFile(page, dir, "lists.docx", Buffer.from(await buildDocx(["One", "Two", "Three", "Four"])));
  const { frame, text } = await openEditor(page, id, "lists.docx");
  const bullets = page.getByRole("button", { name: "Bulleted list" });

  // One and Two become a bulleted list; Tab at the start of Two moves it a level in
  await frame.getByText("One").click();
  await frame.getByText("Two").click({ modifiers: ["Shift"] });
  await bullets.click();
  await expect(frame.locator(".tf-docx-marker")).toHaveCount(2);
  await frame.getByText("Two").click();
  await text.press("Home");
  await text.press("Tab");
  await expect(frame.getByText("Two")).toBeVisible();
  await frame.getByText("One").click();
  await expect(bullets).toHaveAttribute("aria-pressed", "true");

  // Three and Four become a numbered list, then Four is taken out of it
  await frame.getByText("Three").click();
  await frame.getByText("Four").click({ modifiers: ["Shift"] });
  await page.getByRole("button", { name: "Numbered list" }).click();
  await expect(frame.locator(".tf-docx-marker")).toHaveCount(4);
  await frame.getByText("Four").click();
  await page.getByRole("button", { name: "Numbered list" }).click();
  await expect(frame.locator(".tf-docx-marker")).toHaveCount(3);

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { zip, xml } = await savedXml(page, id);
  const items = [...xml.matchAll(/<w:p>(.*?)<\/w:p>/g)].map((m) => {
    const num = /<w:numPr><w:ilvl w:val="(\d)"\/><w:numId w:val="(\d+)"\/><\/w:numPr>/.exec(m[1]);
    return `${/<w:t[^>]*>([^<]*)<\/w:t>/.exec(m[1])?.[1]}:${num ? `${num[2]}/${num[1]}` : "-"}`;
  });
  expect(items).toEqual(["One:1/0", "Two:1/1", "Three:2/0", "Four:-"]);
  const numbering = await zip.file("word/numbering.xml")!.async("string");
  expect(numbering).toContain('<w:numFmt w:val="bullet"/>');
  expect(numbering).toContain('<w:numFmt w:val="decimal"/>');
  expect(await zip.file("word/_rels/document.xml.rels")!.async("string")).toContain('Target="numbering.xml"');
  expect(await zip.file("[Content_Types].xml")!.async("string")).toContain('PartName="/word/numbering.xml"');
});

test("fonts and line spacing are changed from the toolbar, and the file gets them", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word fonts");
  const body = "<w:p><w:r><w:t>Hello world</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p><w:p><w:r><w:t>Third</w:t></w:r></w:p>";
  const id = await uploadFile(page, dir, "fonts.docx", Buffer.from(await buildDocument(body)));
  const { frame } = await openEditor(page, id, "fonts.docx");
  const font = page.getByLabel("Font", { exact: true });
  const spacing = page.getByRole("button", { name: "Line and paragraph spacing" });

  // A Latin font for "Second", an East Asian one for "Third" (the view opens again on the changed document)
  await frame.getByText("Second").dblclick();
  await font.selectOption("Georgia");
  await expect(font).toHaveValue("Georgia");
  await frame.getByText("Third").dblclick();
  await font.selectOption("Microsoft JhengHei");
  await expect(font).toHaveValue("Microsoft JhengHei");

  // Line spacing of 1.5 and 12 points before for the first paragraph; the menu then shows them
  await frame.getByText("Hello world").click();
  await spacing.click();
  await page.getByRole("menuitemradio", { name: "1.5" }).click();
  await expect(frame.getByText("Hello world")).toBeVisible();
  await spacing.click();
  await expect(page.getByRole("menuitemradio", { name: "1.5" })).toHaveAttribute("aria-checked", "true");
  await page.getByRole("menuitem", { name: "Add space before paragraph" }).click();
  await expect(frame.getByText("Hello world")).toBeVisible();
  await spacing.click();
  await expect(page.getByRole("menuitem", { name: "Remove space before paragraph" })).toBeVisible();
  await page.getByRole("menu").press("Escape");
  await expect(page.getByRole("menu")).toBeHidden();

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { xml } = await savedXml(page, id);
  const paragraphs = [...xml.matchAll(/<w:p>(.*?)<\/w:p>/g)].map((m) => m[1]);
  expect(paragraphs.find((p) => p.includes("Hello"))).toMatch(/<w:pPr><w:spacing w:line="360" w:lineRule="auto" w:before="240"\/><\/w:pPr><w:r><w:t>Hello world<\/w:t>/);
  expect(paragraphs.find((p) => p.includes("Second"))).toContain('<w:rPr><w:rFonts w:ascii="Georgia" w:hAnsi="Georgia" w:cs="Georgia"/></w:rPr><w:t>Second</w:t>');
  expect(paragraphs.find((p) => p.includes("Third"))).toContain('<w:rFonts w:ascii="Microsoft JhengHei" w:hAnsi="Microsoft JhengHei" w:eastAsia="Microsoft JhengHei"/>');
});

test("a header and a footer are added and edited, and the document shows them once saved", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Word headers");
  const id = await uploadFile(page, dir, "headers.docx", Buffer.from(await buildDocument("<w:p><w:r><w:t>Body text</w:t></w:r></w:p>")));
  const { frame, text } = await openEditor(page, id, "headers.docx");
  const menu = page.getByRole("button", { name: "Header and footer" });

  // A header added and typed into (the view shows it instead of the body)
  await menu.click();
  await page.getByRole("menuitem", { name: "Add header" }).click();
  await expect(page.getByText("Header: it shows on every page that uses it.", { exact: false })).toBeVisible();
  await expect(frame.getByText("Body text")).toBeHidden();
  await text.click();
  await text.pressSequentially("Annual report");

  // A footer added the same way, from the menu that now names the header; Ctrl+Z right after takes it away again
  await page.getByRole("button", { name: "Header", exact: true }).click();
  await page.getByRole("menuitem", { name: "Add footer" }).click();
  await expect(page.getByText("Footer: it shows on every page that uses it.", { exact: false })).toBeVisible();
  await text.press("ControlOrMeta+z");
  await expect(frame.getByText("Annual report")).toBeVisible();
  await page.getByRole("button", { name: "Header", exact: true }).click();
  await page.getByRole("menuitem", { name: "Add footer" }).click();
  await expect(page.getByText("Footer: it shows on every page that uses it.", { exact: false })).toBeVisible();
  await text.click();
  await text.pressSequentially("Example footer");

  // Back to the header, which kept its text, then to the body
  await page.getByRole("button", { name: "Footer", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "Header" }).click();
  await frame.getByText("Annual report").click();
  await text.press("End");
  await text.pressSequentially(" 2026");
  await page.getByRole("button", { name: "Back to document" }).click();
  await frame.getByText("Body text").click();
  await text.press("End");
  await text.pressSequentially(" here");

  const save = saving(page, id);
  await page.getByRole("button", { name: "Save" }).click();
  expect((await save).ok()).toBe(true);
  const { zip, xml } = await savedXml(page, id);
  expect(xml).toContain("<w:t>Body text here</w:t>");
  expect(xml).toMatch(/<w:sectPr><w:headerReference w:type="default" r:id="rId3"\/><w:footerReference w:type="default" r:id="rId4"\/><w:pgSz /);
  expect(await zip.file("word/header1.xml")!.async("string")).toContain(">Annual report 2026<");
  expect(await zip.file("word/footer1.xml")!.async("string")).toContain(">Example footer<");
  const rels = await zip.file("word/_rels/document.xml.rels")!.async("string");
  expect(rels).toContain('Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"');
  expect(rels).toContain('Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"');
  const types = await zip.file("[Content_Types].xml")!.async("string");
  expect(types).toContain('PartName="/word/header1.xml"');
  expect(types).toContain('PartName="/word/footer1.xml"');

  // The preview shows them on the page
  await page.getByRole("button", { name: "Done editing" }).click();
  const preview = page.frameLocator('iframe[title="headers.docx"]');
  await expect(preview.locator(".tf-docx-hdr").getByText("Annual report 2026")).toBeVisible();
  await expect(preview.locator(".tf-docx-ftr").getByText("Example footer")).toBeVisible();
});
