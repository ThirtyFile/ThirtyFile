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
