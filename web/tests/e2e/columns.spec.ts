// The Columns view: a column for each folder level, opened with the mouse or the keyboard; a preview column for a file;
// large folders a part at a time in a column; selecting and moving items between columns; going back keeps the columns.
import { expect, test, type Page } from "@playwright/test";
import { answer, listing, makeFolder, openFolder, signIn, uploadFile } from "./helpers";

/** Signed in, at a width where the Columns view is offered, and with it chosen */
async function columnsView(page: Page) {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signIn(page);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("columns")));
}

const column = (page: Page, name: string) => page.getByRole("listbox", { name, exact: true });
const item = (page: Page, columnName: string, name: string) => column(page, columnName).getByRole("option", { name, exact: true });
/** The name of what has the focus, and the column it is in */
const focused = (page: Page) =>
  page.evaluate(() => {
    const el = document.activeElement;
    return el?.closest("[data-node-id]") ? `${el.closest("[role=listbox]")?.getAttribute("aria-label")}/${el.textContent}` : null;
  });
const children = async (page: Page, id: string): Promise<{ name: string }[]> => (await page.request.get(`/api/nodes/${id}/children`)).json();
/** A folder's name (makeFolder gives the top folder of a test a random part) */
const nameOf = async (page: Page, id: string): Promise<string> => (await (await page.request.get(`/api/nodes/${id}`)).json()).node.name;

test("folders open in columns to the right, Left and Right go between them, and going back keeps them", async ({ page }) => {
  await columnsView(page);
  const top = await makeFolder(page, "Columns");
  const topName = await nameOf(page, top);
  const a = await makeFolder(page, "Alpha", top);
  await makeFolder(page, "Beta", top);
  const b = await makeFolder(page, "Bravo", a);
  await uploadFile(page, a, "about.txt", "about");
  await uploadFile(page, b, "brief.txt", "brief");
  await uploadFile(page, b, "budget.txt", "budget");
  await openFolder(page, top);

  // The open folder's column, then the folder selected in it, in the next column
  await expect(column(page, topName)).toBeVisible();
  await item(page, topName, "Alpha").click();
  await expect(item(page, "Alpha", "Bravo")).toBeVisible();
  await expect(item(page, "Alpha", "about.txt")).toBeVisible();

  // Right: into the folder, its first item selected and focused, and its folder opened next to it
  await page.keyboard.press("ArrowRight");
  await page.waitForURL(`**/files/${a}`);
  await expect(item(page, "Alpha", "Bravo")).toHaveAttribute("aria-selected", "true");
  await expect.poll(() => focused(page)).toBe("Alpha/Bravo");
  await expect(item(page, "Bravo", "brief.txt")).toBeVisible();
  // Moving to another column is announced
  // (the first column is the space's: My files)
  await expect(page.getByRole("status").filter({ hasText: "Column 3: Alpha" })).toBeAttached();
  await page.keyboard.press("ArrowRight");
  await page.waitForURL(`**/files/${b}`);
  await expect.poll(() => focused(page)).toBe("Bravo/brief.txt");
  // A file selected: a preview column with its details
  const preview = page.getByRole("region", { name: "Preview" });
  await expect(preview.getByRole("heading", { name: "brief.txt" })).toBeVisible();
  await expect(preview.getByRole("button", { name: "Open" })).toBeVisible();
  // Typing the start of a name goes to it
  await page.keyboard.type("bu");
  await expect(item(page, "Bravo", "budget.txt")).toHaveAttribute("aria-selected", "true");
  await expect(preview.getByRole("heading", { name: "budget.txt" })).toBeVisible();

  // Left: back to the column before, with the folder came from selected, and the columns beyond it kept
  await page.keyboard.press("ArrowLeft");
  await page.waitForURL(`**/files/${a}`);
  await expect.poll(() => focused(page)).toBe("Alpha/Bravo");
  await expect(item(page, "Bravo", "budget.txt")).toBeVisible();
  await page.keyboard.press("ArrowLeft");
  await page.waitForURL(`**/files/${top}`);
  await expect.poll(() => focused(page)).toBe(`${topName}/Alpha`);
  await expect(column(page, "Alpha")).toBeVisible();
  await expect(column(page, "Bravo")).toBeVisible();

  // Right goes back to what was open there; Up and Down move within the column
  await page.keyboard.press("ArrowRight");
  await page.waitForURL(`**/files/${a}`);
  await expect.poll(() => focused(page)).toBe("Alpha/Bravo");
  await page.keyboard.press("ArrowDown");
  await expect(item(page, "Alpha", "about.txt")).toHaveAttribute("aria-selected", "true");
  // Selecting something else replaces the columns to its right
  await expect(column(page, "Bravo")).toHaveCount(0);

  // A click in another column opens its folder with what was clicked selected
  await item(page, topName, "Beta").click();
  await page.waitForURL(`**/files/${top}`);
  await expect(item(page, topName, "Beta")).toHaveAttribute("aria-selected", "true");
  // An empty folder's column isn't a list: a group named after it, which says it is empty
  await expect(page.getByRole("group", { name: "Beta", exact: true })).toContainText("This folder is empty.");
  await expect(column(page, "Alpha")).toHaveCount(0);
});

test("a column can be made wider, and keeps its width", async ({ page }) => {
  await columnsView(page);
  const top = await makeFolder(page, "Widths");
  await makeFolder(page, "Inside", top);
  await openFolder(page, top);
  const handle = page.getByRole("separator", { name: /Resize the "Widths .*" column/ });
  const box = handle.locator("..");
  const before = (await box.boundingBox())!.width;
  await handle.focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect.poll(async () => (await box.boundingBox())!.width).toBe(before + 32);
  await page.reload();
  await expect
    .poll(
      async () =>
        (
          await page
            .getByRole("separator", { name: /Resize the "Widths .*" column/ })
            .locator("..")
            .boundingBox()
        )?.width,
    )
    .toBe(before + 32);
});

test("items selected in a column move to a folder of another column", async ({ page }) => {
  await columnsView(page);
  const top = await makeFolder(page, "Moves");
  const topName = await nameOf(page, top);
  const dest = await makeFolder(page, "Dest", top);
  const src = await makeFolder(page, "Src", top);
  for (const n of ["a.txt", "b.txt", "c.txt", "d.txt"]) await uploadFile(page, src, n, n);
  await openFolder(page, src);

  // Shift extends the selection within the column
  await item(page, "Src", "a.txt").click();
  await page.keyboard.press("Shift+ArrowDown");
  await expect(page.locator("footer")).toContainText("2 items selected");
  // Cut, go to the column before, open the other folder and paste: the shared actions
  await page.keyboard.press("Control+x");
  await page.keyboard.press("ArrowLeft");
  await page.waitForURL(`**/files/${top}`);
  await expect.poll(() => focused(page)).toContain("/Src");
  await page.keyboard.press("ArrowUp");
  await expect.poll(() => focused(page)).toContain("/Dest");
  await page.keyboard.press("Enter");
  await page.waitForURL(`**/files/${dest}`);
  // Pasted once the folder has opened (its search box names it)
  await expect(page.getByRole("textbox", { name: "Search Dest" })).toBeVisible();
  const moved = answer(page, "POST", "/api/nodes/move");
  await page.keyboard.press("Control+v");
  expect((await moved).ok()).toBe(true);
  await expect(item(page, "Dest", "a.txt")).toBeVisible();
  await expect(item(page, "Dest", "b.txt")).toBeVisible();
  expect((await children(page, src)).map((n) => n.name).sort()).toEqual(["c.txt", "d.txt"]);

  // Dragged from one column onto a folder in another
  await openFolder(page, top);
  await item(page, topName, "Src").click();
  await expect(item(page, "Src", "c.txt")).toBeVisible();
  const dropped = answer(page, "POST", "/api/nodes/move");
  await item(page, "Src", "c.txt").dragTo(page.getByRole("option", { name: "Dest", exact: true }));
  expect((await dropped).ok()).toBe(true);
  await expect.poll(async () => (await children(page, dest)).map((n) => n.name).sort()).toEqual(["a.txt", "b.txt", "c.txt"]);
});

/** A folder of `size` folders, made through the API a few at a time */
async function bigFolder(page: Page, parent: string, size: number): Promise<string> {
  const big = await makeFolder(page, "Large", parent);
  const nameOf = (i: number) => `Item ${String(i).padStart(4, "0")}`;
  for (let i = 0; i < size; i += 25) await Promise.all(Array.from({ length: Math.min(25, size - i) }, (_, k) => makeFolder(page, nameOf(i + k), big)));
  return big;
}

test("a large folder loads a part at a time in its column", async ({ page }) => {
  test.setTimeout(120_000);
  await columnsView(page);
  const top = await makeFolder(page, "Big");
  const big = await bigFolder(page, top, 1200);
  await openFolder(page, top);
  const offsets: number[] = [];
  page.on("request", (r) => {
    const url = new URL(r.url());
    if (url.pathname === `/api/nodes/${big}/children` && url.searchParams.has("offset")) offsets.push(Number(url.searchParams.get("offset")));
  });
  const second = page.waitForResponse((r) => {
    const url = new URL(r.url());
    return url.pathname === `/api/nodes/${big}/children` && url.searchParams.get("offset") === "500";
  });
  const first = listing(page, big);
  await page.getByRole("option", { name: "Large", exact: true }).click();
  await first;
  await expect(item(page, "Large", "Item 0000")).toBeVisible();
  // The part in view and the next one, not the part at the end
  await second;
  expect(new Set(offsets)).toEqual(new Set([0, 500]));

  // Into it: End reaches the last item, whose part loads then
  await page.keyboard.press("ArrowRight");
  await page.waitForURL(`**/files/${big}`);
  await expect.poll(() => focused(page)).toBe("Large/Item 0000");
  await page.keyboard.press("End");
  await expect(item(page, "Large", "Item 1199")).toHaveAttribute("aria-selected", "true");
  expect(offsets).toContain(1000);
  // Select all takes the items not loaded too
  await page.keyboard.press("Control+a");
  await expect(page.locator("footer")).toContainText("1,200 items selected");
  // Back in the column before, the large folder is selected and still open next to it
  await page.keyboard.press("Escape");
  await page.keyboard.press("ArrowLeft");
  await page.waitForURL(`**/files/${top}`);
  await expect(page.getByRole("option", { name: "Large", exact: true }).first()).toHaveAttribute("aria-selected", "true");
  await expect(column(page, "Large")).toBeVisible();
});
