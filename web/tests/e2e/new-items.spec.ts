// New items as in File Explorer (the Windows style): a new folder shows at once at the end of the list, being renamed,
// stays there after renaming until F5, takes a name typed before the server answered, and goes again if making it failed
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, signIn } from "./helpers";

const names = (page: Page) => page.locator("[data-node-id]").evaluateAll((rows) => rows.map((r) => r.querySelector("[data-name]")?.textContent ?? r.querySelector("input")?.value));
const box = (page: Page) => page.getByRole("textbox", { name: "New name" });

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
  await folderWith(page, "At the end", ["Alpha", "Zulu"]);
  await page.keyboard.press("Control+Shift+N");
  await expect(box(page)).toBeFocused();
  await expect(box(page)).toHaveValue("New folder");
  expect(await names(page)).toEqual(["Alpha", "Zulu", "New folder"]);
  await box(page).fill("Beta");
  await box(page).press("Enter");
  await expect(page.locator("[data-node-id]").nth(2)).toHaveText(/Beta/);
  // The list loads again after the change: the folder stays where it is, selected, with the focus
  await page.waitForLoadState("networkidle");
  expect(await names(page)).toEqual(["Alpha", "Zulu", "Beta"]);
  const beta = page.locator("[data-node-id]").nth(2);
  await expect(beta).toHaveAttribute("aria-selected", "true");
  await expect(beta).toBeFocused();
  // F5: in its sorted place
  await page.keyboard.press("F5");
  await expect.poll(() => names(page)).toEqual(["Alpha", "Beta", "Zulu"]);

  // A name typed straight after the shortcut goes into the new folder's name
  await page.keyboard.press("Control+Shift+N");
  await page.keyboard.type("Gamma");
  await page.keyboard.press("Enter");
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
  await box(page).press("Enter");
  release();
  await expect.poll(async () => ((await (await page.request.get(`/api/nodes/${dir}/children`)).json()) as { name: string }[]).map((n) => n.name).sort()).toEqual(["Alpha", "Reports"]);
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
