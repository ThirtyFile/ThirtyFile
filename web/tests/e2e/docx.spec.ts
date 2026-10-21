// Word acceptance through the packaged SDK. Assertions describe Office semantics rather than the retired iframe's DOM or XML spelling.
import { test, expect, type Page } from "@playwright/test";
import { buildDocument, UNKNOWN_CONTENT, UNKNOWN_PART } from "../fixtures";
import { signIn, makeFolder, uploadFile, answer } from "./helpers";
import { office, officeCall, officeFile, openOffice, readyOffice } from "./office-sdk.helpers";

test.setTimeout(180_000);
const BODY = "<w:p><w:r><w:t>One Two Three</w:t></w:r></w:p>";
async function start(page: Page, body = BODY) {
  await signIn(page);
  const folder = await makeFolder(page, "Word SDK");
  const id = await uploadFile(page, folder, "document.docx", Buffer.from(await buildDocument(body)));
  await openOffice(page, id, "word");
  return { id, folder, agent: office(page).getByRole("textbox", { name: "Office editor text input" }) };
}
const command = (page: Page, id: string, params: Record<string, unknown> = {}) => officeCall(page, "execute", id, params);
const inspect = (page: Page) => command(page, "document.inspect");
async function caret(page: Page, path: number[], off = 0) {
  const position = { story: "body", path, off };
  await command(page, "select.range", { anchor: position, focus: position });
}
async function save(page: Page, id: string) {
  const response = answer(page, "PUT", `/api/files/${id}/content`);
  await office(page).getByRole("button", { name: "Save", exact: true }).click();
  expect((await response).ok()).toBe(true);
  await expect.poll(async () => (await officeCall(page, "getState")).dirty).toBe(false);
  const zip = await officeFile(page, id);
  return { zip, xml: await zip.file("word/document.xml")!.async("string") };
}
async function contentText(page: Page, xml: string) {
  return page.evaluate((xml) => [...new DOMParser().parseFromString(xml, "application/xml").getElementsByTagNameNS("*", "t")].map((element) => element.textContent).join(""), xml);
}

test("text edits preserve headings, hyperlinks, lists, tables, sections and opaque custom XML", async ({ page }) => {
  const body =
    '<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Quarterly report</w:t></w:r></w:p><w:p><w:hyperlink r:id="rId10"><w:r><w:t>the website</w:t></w:r></w:hyperlink></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First point</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4500"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>North</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>Section end</w:t></w:r></w:p><w:p><w:r><w:t>Last paragraph.</w:t></w:r></w:p>';
  const { id, agent } = await start(page, body);
  await agent.press("Control+End");
  await agent.pressSequentially(" Added.");
  await agent.press("Enter");
  await agent.pressSequentially("A new paragraph");
  const { zip, xml } = await save(page, id);
  const text = await contentText(page, xml);
  for (const value of ["Quarterly report", "the website", "First point", "North", "Last paragraph. Added.", "A new paragraph"]) expect(text).toContain(value);
  expect(xml).toMatch(/<w:pStyle[^>]*Heading1/);
  expect(xml).toContain("<w:hyperlink");
  expect(xml).toContain("<w:numPr>");
  expect(xml).toContain("<w:tbl>");
  expect(xml).toContain('w:val="continuous"');
  expect(await zip.file(UNKNOWN_PART)!.async("string")).toBe(UNKNOWN_CONTENT);
  expect(await zip.file("word/_rels/document.xml.rels")!.async("string")).toContain("https://example.com/");
});

test("a paragraph after a table is not joined through the table by Backspace", async ({ page }) => {
  const body =
    '<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="4500"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>After</w:t></w:r></w:p>';
  const { id, agent } = await start(page, body);
  await caret(page, [2]);
  await agent.press("Backspace");
  await agent.pressSequentially("X");
  const { xml } = await save(page, id);
  expect(xml).toContain("<w:tbl>");
  expect(await contentText(page, xml)).toContain("XAfter");
});

test("unsaved edits survive navigation and discarding leaves the stored document unchanged", async ({ page }) => {
  const { id, folder, agent } = await start(page);
  await agent.press("Control+End");
  await agent.pressSequentially(" kept");
  await expect.poll(async () => (await officeCall(page, "getState")).engine.content.text).toContain(" kept");
  const state = (await officeCall(page, "getState")).engine.stateId;
  await page.getByRole("button", { name: "Open file location", exact: true }).click();
  await page.waitForURL(`**/files/${folder}`);
  await page.goBack();
  await readyOffice(page);
  expect((await officeCall(page, "getState")).engine.stateId).toBe(state);
  await office(page).getByRole("button", { name: "Done editing", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Discard and exit", exact: true }).click();
  await expect(page.getByRole("button", { name: "Edit document", exact: true })).toBeVisible();
  expect(await (await officeFile(page, id)).file("word/document.xml")!.async("string")).not.toContain("kept");
});

test("typing into table cells and Tab keep text in the corresponding cells", async ({ page }) => {
  const { id, agent } = await start(page);
  await command(page, "insert.table", { rows: 2, cols: 2 });
  await agent.pressSequentially("First cell");
  await agent.press("Tab");
  await agent.pressSequentially("Second cell");
  const { xml } = await save(page, id);
  expect(await contentText(page, xml)).toContain("First cellSecond cell");
  expect((await inspect(page)).blocks[0].cells[0]).toEqual(["First cell", "Second cell"]);
});

test("table rows and columns insert, delete and undo through the SDK's native actions", async ({ page }) => {
  const { id, agent } = await start(page);
  await command(page, "insert.table", { rows: 2, cols: 2 });
  await command(page, "table.insertRowBelow");
  await command(page, "table.insertColumnRight");
  expect((await inspect(page)).blocks[0]).toMatchObject({ rows: 3, cols: 3 });
  await agent.press("Control+z");
  await expect.poll(async () => (await inspect(page)).blocks[0].cols).toBe(2);
  await command(page, "table.deleteRow");
  expect((await inspect(page)).blocks[0].rows).toBe(2);
  expect((await save(page, id)).xml).toContain("<w:tbl>");
});

test("selected cells merge, split and undo without losing the table", async ({ page }) => {
  const { id, agent } = await start(page);
  await command(page, "insert.table", { rows: 2, cols: 2 });
  await command(page, "table.selectRow");
  await command(page, "table.merge");
  expect((await inspect(page)).blocks[0].cells[0]).toHaveLength(1);
  await command(page, "table.split", { columns: 2, rows: 1 });
  expect((await inspect(page)).blocks[0].cells[0]).toHaveLength(2);
  await agent.press("Control+z");
  await expect.poll(async () => (await inspect(page)).blocks[0].cells[0].length).toBe(1);
  expect((await save(page, id)).xml).toMatch(/<w:gridSpan[^>]*w:val="2"/);
});

test("table borders, shading and exact column widths persist", async ({ page }) => {
  const { id } = await start(page);
  await command(page, "insert.table", { rows: 2, cols: 2 });
  await command(page, "table.selectRow");
  await command(page, "table.shading", { color: "00AAFF" });
  await command(page, "table.borders", { kind: "all", color: "FF0000", width: 1.5 });
  await command(page, "table.columnWidth", { width: 120 });
  const { xml } = await save(page, id);
  expect(xml).toContain('w:fill="00AAFF"');
  expect(xml).toContain('w:color="FF0000"');
  expect(xml).toMatch(/<w:gridCol[^>]*w:w="2400"/);
});

test("Ribbon formatting and shortcuts preserve the selected text", async ({ page }) => {
  const { id, agent } = await start(page);
  await command(page, "select.text", { text: "One" });
  await office(page).locator('[data-office-chrome] [role="tab"][data-id="tab:Home"]').click();
  await office(page)
    .locator('[aria-label="Office tools"]')
    .getByRole("button", { name: /^(Bold|粗體)$/ })
    .first()
    .click();
  await agent.press("Control+i");
  const { xml } = await save(page, id);
  expect(xml).toMatch(/<w:b(?:\s|\/|>)/);
  expect(xml).toMatch(/<w:i(?:\s|\/|>)/);
  expect(await contentText(page, xml)).toBe("One Two Three");
});

test("range formatting after a new paragraph does not depend on DOM selectionchange", async ({ page }) => {
  const { id, agent } = await start(page);
  await agent.press("Control+End");
  await agent.press("Enter");
  await agent.pressSequentially("New paragraph");
  await command(page, "select.text", { text: "New paragraph" });
  await command(page, "format.bold", { value: true });
  const { xml } = await save(page, id);
  expect(xml).toMatch(/<w:b(?:\s|\/|>)/);
  expect(await contentText(page, xml)).toContain("New paragraph");
});

test("bulleted and numbered paragraphs create a valid numbering part", async ({ page }) => {
  const { id } = await start(page, "<w:p><w:r><w:t>One</w:t></w:r></w:p><w:p><w:r><w:t>Two</w:t></w:r></w:p>");
  await command(page, "select.text", { text: "One" });
  await command(page, "para.bullets");
  await command(page, "select.text", { text: "Two" });
  await command(page, "para.numbering");
  const { zip, xml } = await save(page, id);
  expect((xml.match(/<w:numPr>/g) ?? []).length).toBe(2);
  expect(await zip.file("word/numbering.xml")!.async("string")).toContain("<w:num");
});

test("font family, point size and paragraph spacing survive export", async ({ page }) => {
  const { id } = await start(page);
  await command(page, "select.text", { text: "One Two Three" });
  await command(page, "format.font", { name: "Inter" });
  await command(page, "format.size", { size: 22 });
  await command(page, "para.lineSpacing", { value: 1.5 });
  await command(page, "para.spacing", { before: 12, after: 18 });
  const { xml } = await save(page, id);
  for (const value of ['w:ascii="Inter"', 'w:val="44"', 'w:line="360"', 'w:before="240"', 'w:after="360"']) expect(xml).toContain(value);
});

test("headers and footers are editable and exported as their own parts", async ({ page }) => {
  const { id } = await start(page);
  await command(page, "insert.header", { text: "Annual report 2026" });
  await command(page, "insert.closeHeader");
  await command(page, "insert.footer", { text: "Example footer" });
  await command(page, "insert.closeHeader");
  const { zip } = await save(page, id);
  const names = Object.keys(zip.files);
  const headers = await Promise.all(names.filter((name) => /^word\/header.*\.xml$/.test(name)).map((name) => zip.file(name)!.async("string")));
  const footers = await Promise.all(names.filter((name) => /^word\/footer.*\.xml$/.test(name)).map((name) => zip.file(name)!.async("string")));
  expect(headers.join("")).toContain("Annual report 2026");
  expect(footers.join("")).toContain("Example footer");
});

test("page number fields are retained in the footer", async ({ page }) => {
  const { id } = await start(page);
  await command(page, "insert.footer", { text: "Page " });
  await command(page, "insert.pageNumber", { position: "bottom", format: "x of y" });
  await command(page, "insert.closeHeader");
  const { zip } = await save(page, id);
  const footers = await Promise.all(
    Object.keys(zip.files)
      .filter((name) => /^word\/footer.*\.xml$/.test(name))
      .map((name) => zip.file(name)!.async("string")),
  );
  expect(footers.join("")).toMatch(/\bPAGE\b/);
  expect(footers.join("")).toContain("NUMPAGES");
});

test("different first and even pages keep independent headers", async ({ page }) => {
  const { id } = await start(page);
  await command(page, "insert.pageBreak");
  await command(page, "text.insert", { text: "Second page" });
  await command(page, "insert.pageBreak");
  await command(page, "text.insert", { text: "Third page" });
  await command(page, "insert.header", { text: "Every page" });
  await command(page, "insert.closeHeader");
  await command(page, "layout.differentFirstPage", { value: true });
  await command(page, "layout.differentOddEven", { value: true });
  await command(page, "insert.editHeader", { page: 0 });
  await command(page, "text.insert", { text: "Cover" });
  await command(page, "insert.closeHeader");
  await command(page, "select.text", { text: "Second page" });
  await command(page, "insert.header", { text: "Even" });
  await command(page, "insert.closeHeader");
  const headers = (await inspect(page)).sections[0].props.headers;
  expect(headers.default).not.toBeNull();
  expect(headers.first).not.toBeNull();
  expect(headers.even).not.toBeNull();
  const { zip, xml } = await save(page, id);
  expect(xml).toContain("titlePg");
  expect(await zip.file("word/settings.xml")!.async("string")).toContain("evenAndOddHeaders");
});

test("typing, undo and immediate save after a native formatting action lose nothing", async ({ page }) => {
  const { id, agent } = await start(page);
  await command(page, "para.bullets");
  await agent.press("Control+z");
  await agent.press("Control+End");
  await agent.pressSequentially(" Typed");
  const { xml } = await save(page, id);
  expect(await contentText(page, xml)).toContain("One Two Three Typed");
  expect(xml).not.toContain("<w:numPr>");
});
