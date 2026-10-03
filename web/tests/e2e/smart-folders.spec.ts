// Smart folders: a search saved as one from the search page, opened from the navigation pane with the items it finds
// (which work as anywhere), changed with the keyboard, and deleted without touching its items. Each test signs in as an
// account of its own: smart folders are each person's own.
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, signInAsNewUser, unique, uploadFile } from "./helpers";

/** The row of the item named exactly `name` */
const row = (page: Page, name: string) => page.locator("[data-node-id]").filter({ has: page.getByText(name, { exact: true }) });
const nav = (page: Page) => page.getByRole("navigation", { name: "File locations" });

test("a search is saved as a smart folder that lists what matches, and its items work as anywhere", async ({ page }) => {
  await signInAsNewUser(page, "smart");
  const u = unique();
  const dir = await makeFolder(page, "Smart");
  const inner = await makeFolder(page, "Inner", dir);
  await uploadFile(page, dir, `report-${u}.pdf`, "r");
  await uploadFile(page, dir, `report-${u}.txt`, "t");
  const old = await uploadFile(page, inner, `old report-${u}.pdf`, "o");
  const elsewhereName = `Elsewhere ${u}`;
  const elsewhere = await makeFolder(page, elsewhereName, (await (await page.request.get("/api/auth/me")).json()).root_id);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("details")));

  // A search in the folder for documents, saved as a smart folder: the dialog starts from the search
  const searched = answer(page, "GET", "/api/search");
  await page.goto(`/search?q=report-${u}&in=${dir}&type=doc`);
  expect((await searched).ok()).toBe(true);
  await expect(row(page, `old report-${u}.pdf`)).toBeVisible();
  await page.getByRole("button", { name: "Save as smart folder" }).click();
  const dialog = page.getByRole("dialog", { name: "New smart folder" });
  const type = dialog.getByLabel("Type", { exact: true });
  await expect(dialog.getByLabel("Name", { exact: true })).toHaveValue(`report-${u}`);
  await expect(dialog.getByLabel("Name contains")).toHaveValue(`report-${u}`);
  await expect(dialog.getByLabel("Look in")).toHaveValue(`folder:${dir}`);
  await expect(type).toHaveValue("doc");
  // Only PDFs
  await type.selectOption("custom");
  await dialog.getByLabel("File types").fill("pdf");
  await dialog.getByLabel("Name", { exact: true }).fill(`Reports ${u}`);
  const made = answer(page, "POST", "/api/smart-folders");
  // Rows also present in the preceding search do not prove the smart folder has loaded.
  const matches = answer(page, "GET", /^\/api\/smart-folders\/\d+\/items$/);
  await dialog.getByRole("button", { name: "Create" }).click();
  expect((await made).ok()).toBe(true);
  await page.waitForURL(/\/smart\/\d+$/);
  expect((await matches).ok()).toBe(true);

  // It lists the matches wherever they are below the folder, with where each is, and says what it looks for
  await expect(row(page, `report-${u}.pdf`)).toBeVisible();
  await expect(row(page, `old report-${u}.pdf`)).toContainText("Inner");
  await expect(row(page, `report-${u}.txt`)).toHaveCount(0);
  await expect(page.getByRole("group", { name: "Smart folder" }).getByRole("list", { name: "Looks for" })).toContainText("File types: pdf");
  await expect(nav(page).getByRole("link", { name: `Reports ${u}` })).toHaveAttribute("aria-current", "page");

  // Its items have the usual actions: renamed in place, it stays (still a match)
  await row(page, `old report-${u}.pdf`).click();
  await page.keyboard.press("F2");
  const box = page.getByRole("textbox", { name: "New name" });
  await box.fill(`older report-${u}.pdf`);
  const renamed = answer(page, "PATCH", `/api/nodes/${old}`);
  await box.press("Enter");
  expect((await renamed).ok()).toBe(true);
  await expect(row(page, `older report-${u}.pdf`)).toBeVisible();
  // Open file location goes to the folder it is in
  await row(page, `older report-${u}.pdf`).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Open file location" }).click();
  await page.waitForURL(`/files/${inner}`);
  await expect(row(page, `older report-${u}.pdf`)).toBeVisible();

  // Moved out of the folder it looks in (cut there, and pasted in a folder elsewhere), the item leaves the smart folder:
  // it holds no items of its own
  await row(page, `older report-${u}.pdf`).click();
  await page.keyboard.press("Control+x");
  await nav(page).getByRole("treeitem", { name: elsewhereName }).click();
  await page.waitForURL(`/files/${elsewhere}`);
  const moved = answer(page, "POST", "/api/nodes/move");
  await page.getByRole("button", { name: "Paste" }).click();
  expect((await moved).ok()).toBe(true);
  await expect(row(page, `older report-${u}.pdf`)).toBeVisible();
  const listed = answer(page, "GET", /^\/api\/smart-folders\/\d+\/items$/);
  await nav(page)
    .getByRole("link", { name: `Reports ${u}` })
    .click();
  expect((await listed).ok()).toBe(true);
  await expect(row(page, `report-${u}.pdf`)).toBeVisible();
  await expect(row(page, `older report-${u}.pdf`)).toHaveCount(0);
});

test("a smart folder is changed with the keyboard and deleted, and its items stay", async ({ page }) => {
  await signInAsNewUser(page, "smart-edit");
  const u = unique();
  const dir = await makeFolder(page, "Smart edits");
  await uploadFile(page, dir, `a-${u}.txt`, "a");
  await uploadFile(page, dir, `b-${u}.pdf`, "b");
  const res = await page.request.post("/api/smart-folders", { data: { name: `Mine ${u}`, query: { name: u, scope: { kind: "folder", id: dir } } } });
  expect(res.ok()).toBe(true);
  const smart = await res.json();
  await page.goto(`/smart/${smart.id}`);
  await expect(row(page, `a-${u}.txt`)).toBeVisible();
  await expect(row(page, `b-${u}.pdf`)).toBeVisible();

  // Edit… from the bar above the list, with the keyboard
  await page.getByRole("button", { name: "Edit…" }).focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Edit smart folder" });
  const name = dialog.getByLabel("Name", { exact: true });
  await expect(name).toBeFocused();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type(`PDFs ${u}`);
  const type = dialog.getByLabel("Type", { exact: true });
  await type.focus();
  await type.selectOption("custom");
  await type.focus();
  await page.keyboard.press("Tab");
  await expect(dialog.getByLabel("File types")).toBeFocused();
  await page.keyboard.type("pdf");
  const saved = answer(page, "PATCH", `/api/smart-folders/${smart.id}`);
  await page.keyboard.press("Enter");
  expect((await saved).ok()).toBe(true);
  await expect(dialog).toHaveCount(0);
  await expect(row(page, `b-${u}.pdf`)).toBeVisible();
  await expect(row(page, `a-${u}.txt`)).toHaveCount(0);
  await expect(nav(page).getByRole("link", { name: `PDFs ${u}` })).toBeVisible();

  // Deleted from the navigation pane's menu: the page goes back to the files, and the items are all still there
  await nav(page)
    .getByRole("link", { name: `PDFs ${u}` })
    .click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete smart folder" }).click();
  const deleted = answer(page, "DELETE", `/api/smart-folders/${smart.id}`);
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  expect((await deleted).ok()).toBe(true);
  await page.waitForURL(/\/files/);
  await expect(nav(page).getByRole("link", { name: `PDFs ${u}` })).toHaveCount(0);
  const listed = await (await page.request.get(`/api/nodes/${dir}/children`)).json();
  expect(listed.map((n: { name: string }) => n.name).sort()).toEqual([`a-${u}.txt`, `b-${u}.pdf`]);
  expect((await page.request.get(`/api/smart-folders/${smart.id}`)).status()).toBe(404);
});
