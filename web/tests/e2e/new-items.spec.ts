// New items as in File Explorer (the Windows style): a new folder shows at once at the end of the list, being renamed,
// stays there after renaming until F5, takes a name typed before the server answered, and goes again if making it failed
import { expect, test, type Page } from "@playwright/test";
import { answer, listing, makeFolder, signIn } from "./helpers";

const names = (page: Page) => page.locator("[data-node-id]").evaluateAll((rows) => rows.map((r) => r.querySelector("[data-name]")?.textContent ?? r.querySelector("input")?.value));
const box = (page: Page) => page.getByRole("textbox", { name: "New name" });
/** The server's answer to a rename: it may wait its turn behind other tests' changes */
const renamed = (page: Page) => answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);

async function folderWith(page: Page, name: string, items: string[]) {
  await signIn(page);
  const dir = await makeFolder(page, name);
  for (const item of items) await makeFolder(page, item, dir);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await page.goto(`/files/${dir}`);
  await expect(page.locator("[data-node-id]")).toHaveCount(items.length);
  return dir;
}

test("a new folder shows at the end being renamed, and stays there after renaming until F5", async ({ page }) => {
  const dir = await folderWith(page, "At the end", ["Alpha", "Zulu"]);
  await page.keyboard.press("Control+Shift+N");
  await expect(box(page)).toBeFocused();
  await expect(box(page)).toHaveValue("New folder");
  expect(await names(page)).toEqual(["Alpha", "Zulu", "New folder"]);
  await box(page).fill("Beta");
  const done = renamed(page);
  const listed = listing(page, dir, done);
  await box(page).press("Enter");
  await expect(page.locator("[data-node-id]").nth(2)).toHaveText(/Beta/);
  expect((await done).ok()).toBe(true);
  // The list loads again after the change: the folder stays where it is, selected, with the focus
  await listed;
  expect(await names(page)).toEqual(["Alpha", "Zulu", "Beta"]);
  const beta = page.locator("[data-node-id]").nth(2);
  await expect(beta).toHaveAttribute("aria-selected", "true");
  await expect(beta).toBeFocused();
  // F5: in its sorted place
  await page.keyboard.press("F5");
  await expect.poll(() => names(page)).toEqual(["Alpha", "Beta", "Zulu"]);

  // A name typed straight after the shortcut goes into the new folder's name
  const gamma = renamed(page);
  await page.keyboard.press("Control+Shift+N");
  await page.keyboard.type("Gamma");
  await page.keyboard.press("Enter");
  expect((await gamma).ok()).toBe(true);
  await expect.poll(() => names(page)).toEqual(["Alpha", "Beta", "Zulu", "Gamma"]);
});

test("a name typed before the server has made the folder is given to it once it is made", async ({ page }) => {
  const dir = await folderWith(page, "Slow create", ["Alpha"]);
  let release!: () => void;
  const held = new Promise<void>((r) => (release = r));
  await page.route("**/api/folders", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    await held;
    await route.continue();
  });
  await page.getByRole("button", { name: "New", exact: true }).click();
  await page.getByRole("menuitem", { name: /^New folder/ }).click();
  // Shown at once, while the server hasn't answered
  await expect(box(page)).toBeFocused();
  await box(page).fill("Reports");
  const done = renamed(page);
  await box(page).press("Enter");
  release();
  expect((await done).ok()).toBe(true);
  expect(((await (await page.request.get(`/api/nodes/${dir}/children`)).json()) as { name: string }[]).map((n) => n.name).sort()).toEqual(["Alpha", "Reports"]);
  await expect.poll(() => names(page)).toEqual(["Alpha", "Reports"]);
});

test("a folder the server couldn't make goes from the list, and the reason is shown", async ({ page }) => {
  const dir = await folderWith(page, "Failed create", ["Alpha"]);
  await page.route("**/api/folders", (route) =>
    route.request().method() === "POST" ? route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: "Folder not found" }) }) : route.continue(),
  );
  await page.keyboard.press("Control+Shift+N");
  await expect(page.getByText("Folder not found")).toBeVisible();
  await expect(box(page)).toHaveCount(0);
  expect(await names(page)).toEqual(["Alpha"]);
  expect(((await (await page.request.get(`/api/nodes/${dir}/children`)).json()) as unknown[]).length).toBe(1);
});
