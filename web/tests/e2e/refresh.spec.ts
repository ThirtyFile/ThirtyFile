// After a change, only the lists it touched load again: a new favorite reloads no folder, a rename only its own, and
// items moved or put in the trash leave the list they were in at once.
import { expect, test, type Page } from "@playwright/test";
import { signIn } from "./helpers";

async function folder(page: Page, name: string, parent?: string): Promise<string> {
  const parentId = parent ?? (await (await page.request.get("/api/auth/me")).json()).root_id;
  const res = await page.request.post("/api/folders", { data: { parent_id: parentId, name: parent ? name : `${name} ${Date.now().toString(36)}` } });
  expect(res.ok()).toBe(true);
  return (await res.json()).id;
}

/** The folder listings the page asks for from now on, by folder id (not the navigation pane's lists of folders) */
function listings(page: Page) {
  const asked: string[] = [];
  page.on("request", (r) => {
    const m = /\/api\/nodes\/([^/]+)\/children/.exec(r.url());
    if (m && !r.url().includes("folders_only")) asked.push(m[1]);
  });
  return {
    of: (id: string) => asked.filter((x) => x === id).length,
    clear: () => void asked.splice(0),
  };
}

test("a new favorite and a rename reload only what they changed", async ({ page }) => {
  await signIn(page);
  const top = await folder(page, "Refresh");
  const here = await folder(page, "Here", top);
  const elsewhere = await folder(page, "Elsewhere", top);
  for (const name of ["Alpha", "Beta", "Gamma"]) await folder(page, name, here);
  await folder(page, "Other", elsewhere);
  // The other folder was opened before, so its list is kept in the page
  await page.goto(`/files/${elsewhere}`);
  await expect(page.locator("[data-node-id]").filter({ hasText: "Other" })).toBeVisible();
  await page.goto(`/files/${here}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await expect(row("Gamma")).toBeVisible();
  await page.waitForLoadState("networkidle");
  const asked = listings(page);

  await row("Alpha").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Add to favorites" }).click();
  await expect(row("Alpha").getByLabel("Favorite")).toBeVisible();
  await page.waitForLoadState("networkidle");
  expect(asked.of(here)).toBe(0);
  expect(asked.of(elsewhere)).toBe(0);

  await row("Beta").click();
  await page.keyboard.press("F2");
  await page.keyboard.press("End");
  await page.keyboard.type(" 2");
  await page.keyboard.press("Enter");
  await expect(row("Beta 2")).toBeVisible();
  await page.waitForLoadState("networkidle");
  // The renamed item's folder is put in order again; the other folder isn't loaded
  expect(asked.of(here)).toBe(1);
  expect(asked.of(elsewhere)).toBe(0);
  // The star stays (the rename's answer doesn't say it)
  await expect(row("Alpha").getByLabel("Favorite")).toBeVisible();
});

test("items put in the trash or moved leave the list at once, without it loading again", async ({ page }) => {
  await signIn(page);
  const top = await folder(page, "Leave");
  const here = await folder(page, "Here", top);
  const there = await folder(page, "There", top);
  for (const name of ["One", "Two", "Three"]) await folder(page, name, here);
  await page.goto(`/files/${here}`);
  const row = (name: string) => page.locator("[data-node-id]").filter({ hasText: name });
  await expect(row("Three")).toBeVisible();
  await page.waitForLoadState("networkidle");
  const asked = listings(page);

  await row("One").click();
  await page.keyboard.press("Delete");
  await page.getByRole("dialog").getByRole("button", { name: "Move to trash" }).click();
  await expect(row("One")).toHaveCount(0);
  await page.waitForLoadState("networkidle");
  expect(asked.of(here)).toBe(0);

  // Cut here, paste in the other folder: the item is there, and gone from here when coming back
  await row("Two").click();
  await page.keyboard.press("Control+x");
  // Within the page (the clipboard is the page's own)
  await page.getByRole("treeitem", { name: "There" }).click();
  await page.waitForURL(`**/files/${there}`);
  await expect(page.getByText("No files here yet")).toBeVisible();
  await page.getByRole("button", { name: "Paste" }).click();
  await expect(row("Two")).toBeVisible();
  await page.goBack();
  await expect(row("Three")).toBeVisible();
  await expect(row("Two")).toHaveCount(0);
});
