// Searching inside files: turned on in Control panel › General, files read in the background, found by what they hold with
// the match shown, and never found by someone who can't open them
import { expect, test } from "@playwright/test";
import { answer, asAdmin, makeFolder, signIn, signInAsNewUser, unique, uploadFile } from "./helpers";

test("an administrator turns on searching inside files, and a file is found by its words, with a snippet, only by who can open it", async ({ page, browser }, info) => {
  const word = `zebramango${unique()}`;
  await signIn(page);
  try {
    await page.goto("/admin/general");
    const toggle = page.getByRole("switch", { name: "Search inside files" });
    await expect(toggle).toBeVisible();
    const saved = answer(page, "PUT", "/api/admin/content-search");
    await toggle.click();
    expect((await saved).ok()).toBe(true);
    await expect(page.getByRole("button", { name: "Index again" })).toBeVisible();

    const folder = await makeFolder(page, "Minutes");
    await uploadFile(page, folder, "board-minutes.txt", `The board agreed that the ${word} programme starts in May.\n`);
    await uploadFile(page, folder, "picture.jpg", `${word} but not text`);
    // A PDF is read by its text layer
    const pdf = `%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>
endobj
4 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
endobj
5 0 obj
<< /Length 70 >>
stream
BT /F1 12 Tf 72 700 Td (Tender notice for ${word}) Tj ET
endstream
endobj
trailer
<< /Root 1 0 R >>
%%EOF
`;
    await uploadFile(page, folder, "tender.pdf", pdf);
    // Read in the background: the page follows how far it has got
    await expect
      .poll(
        async () => {
          const info = await (await page.request.get("/api/admin/content-search")).json();
          const found = await (await page.request.get(`/api/search?q=${word}&content=true`)).json();
          return { queued: info.queued, indexed: info.indexed, found: found.items.length };
        },
        { timeout: 60_000 },
      )
      .toMatchObject({ queued: 0, found: 2 });

    // The address bar's search looks at names; the option adds what is inside
    await page.goto(`/search?q=${word}`);
    await expect(page.getByText("No matching files found")).toBeVisible();
    // The box is kept in the address, so it is ticked once the address has changed
    await page.getByLabel("Also search inside files").click();
    await expect(page.getByLabel("Also search inside files")).toBeChecked();
    await expect(page.getByRole("row", { name: /board-minutes\.txt/ })).toBeVisible();
    await expect(page.getByRole("columnheader", { name: "Found in" })).toBeVisible();
    await expect(page.locator("mark", { hasText: word })).toHaveCount(2);
    await expect(page.getByRole("row", { name: /tender\.pdf/ })).toBeVisible();
    await expect(page.getByText(/agreed that the/)).toBeVisible();
    await expect(page).toHaveURL(/content=1/);
    // Not the picture, which holds the word but is not read
    await expect(page.getByRole("row", { name: /picture\.jpg/ })).toHaveCount(0);

    // Someone with nothing shared finds nothing, by name or by what is inside
    const context = await browser.newContext({ baseURL: info.project.use.baseURL });
    try {
      const other = await context.newPage();
      await signInAsNewUser(other, "reader");
      await other.goto(`/search?q=${word}&content=1`);
      await expect(other.getByText("No matching files found")).toBeVisible();
      const raw = await (await other.request.get(`/api/search?q=${word}&content=true`)).json();
      expect(raw.items).toEqual([]);
      expect(raw.snippets).toEqual({});
    } finally {
      await context.close();
    }
  } finally {
    // Put back as it was: turning it off deletes what was read
    await asAdmin(browser, info, async (request) => {
      const off = await request.put("/api/admin/content-search", { data: { enabled: false, paused: false, max_file_mb: 25, max_total_mb: 500 } });
      expect(off.ok()).toBe(true);
    });
  }
});
