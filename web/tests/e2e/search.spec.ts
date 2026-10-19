// Searching as you type: the box keeps the focus and the caret when the search page opens, the filters stay while the
// words change, and clearing or Back returns to where the search started
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, openFolder, signInAsNewUser, unique, uploadFile } from "./helpers";

const box = (page: Page) => page.getByRole("textbox", { name: /^Search/ });
/** What has the focus: a text box's text and caret, else the element's tag */
const focus = (page: Page) =>
  page.evaluate(() => {
    const el = document.activeElement;
    return el instanceof HTMLInputElement ? { value: el.value, caret: el.selectionStart, end: el.selectionEnd } : { tag: el?.tagName };
  });
const searched = (page: Page) => answer(page, "GET", "/api/search");

async function folderWithFiles(page: Page, style: "windows" | "mac") {
  await signInAsNewUser(page, "search");
  if (style === "mac") expect((await page.request.put("/api/auth/style", { data: { style: "mac" } })).ok()).toBe(true);
  const dir = await makeFolder(page, "Find here");
  const word = `alpha${unique()}`;
  await uploadFile(page, dir, `${word}-1.txt`, "one");
  await uploadFile(page, dir, `${word}-2.txt`, "two");
  await openFolder(page, dir);
  return { dir, word };
}

for (const [style, phone] of [
  ["windows", false],
  ["windows", true],
  ["mac", false],
] as const) {
  test(`typing goes on in the box when the search page opens (${style} style${phone ? ", phone" : ""})`, async ({ page }) => {
    await page.setViewportSize(phone ? { width: 390, height: 844 } : { width: 1280, height: 720 });
    const { dir, word } = await folderWithFiles(page, style);
    await box(page).click();
    // The first letter, searched after the pause: the search page opens with the box focused, the caret after it
    const first = searched(page);
    await page.keyboard.type(word[0]);
    expect((await first).ok()).toBe(true);
    await expect(page).toHaveURL(new RegExp(`/search\\?q=${word[0]}&in=${dir}`));
    expect(await focus(page)).toEqual({ value: word[0], caret: 1, end: 1 });
    // Then the others, without clicking the box again
    const rest = searched(page);
    await page.keyboard.type(word.slice(1, 5));
    await rest;
    await expect(page).toHaveURL(new RegExp(`q=${word.slice(0, 5)}&`));
    expect(await focus(page)).toEqual({ value: word.slice(0, 5), caret: 5, end: 5 });
    // An edit inside the words, and the results follow it
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("Backspace");
    expect(await focus(page)).toEqual({ value: word.slice(0, 2) + word.slice(3, 5), caret: 2, end: 2 });
    await page.keyboard.type(word[2]);
    await page.keyboard.press("End");
    await page.keyboard.type(word.slice(5));
    await expect(page).toHaveURL(new RegExp(`q=${word}&`));
    await expect(page.locator("[data-node-id]").filter({ hasText: `${word}-1.txt` })).toBeVisible();
    expect(await focus(page)).toEqual({ value: word, caret: word.length, end: word.length });
  });
}

test("editing the words keeps the filters; Back, clearing, Escape and deleting the last letter return to the folder", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 });
  const { dir, word } = await folderWithFiles(page, "windows");
  const folderUrl = new RegExp(`/files/${dir}$`);
  await box(page).fill(word);
  await expect(page).toHaveURL(/\/search\?/);
  await page.getByRole("group", { name: "Search filters" }).getByLabel("Date modified").selectOption({ index: 1 });
  await expect(page).toHaveURL(/date=/);
  const date = new URL(page.url()).searchParams.get("date");
  await box(page).press("End");
  await box(page).press("Backspace");
  await expect(page).toHaveURL(new RegExp(`q=${word.slice(0, -1)}&`));
  const url = new URL(page.url());
  expect([url.searchParams.get("in"), url.searchParams.get("date")]).toEqual([dir, date]);

  // Back: the folder, in one step however many letters were searched
  await page.getByRole("button", { name: "Back", exact: true }).click();
  await expect(page).toHaveURL(folderUrl);

  // The clear button: back to the folder, the box empty and still focused for a new search
  await box(page).fill(word);
  await expect(page).toHaveURL(/\/search\?/);
  await page.getByRole("button", { name: "Clear search", exact: true }).click();
  await expect(page).toHaveURL(folderUrl);
  await expect.poll(() => focus(page)).toEqual({ value: "", caret: 0, end: 0 });

  // Escape, and deleting the last letter
  await page.keyboard.type(word);
  await expect(page).toHaveURL(/\/search\?/);
  await page.keyboard.press("Escape");
  await expect(page).toHaveURL(folderUrl);
  await page.keyboard.type("a");
  await expect(page).toHaveURL(/\/search\?q=a&/);
  await page.keyboard.press("Backspace");
  await expect(page).toHaveURL(folderUrl);
  await expect(box(page)).toHaveValue("");
});

test("Enter searches at once, without waiting for a pause", async ({ page }) => {
  await page.clock.install();
  const { word } = await folderWithFiles(page, "windows");
  // The page's time stands still: a search waiting for a pause would never run
  await page.clock.pauseAt(new Date(Date.now() + 60_000));
  await box(page).click();
  await page.keyboard.type(word);
  expect(new URL(page.url()).pathname).not.toBe("/search");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(new RegExp(`/search\\?q=${word}&`));
  await expect(box(page)).toBeFocused();
});

test("a search opened from a link leaves the focus alone, and clearing it asks for words", async ({ page }) => {
  const { word } = await folderWithFiles(page, "windows");
  // A tab of its own: nowhere to go back to
  const other = await page.context().newPage();
  await other.addInitScript(() => {
    for (const key of Object.keys(localStorage)) if (key.startsWith("tf-tabs")) localStorage.removeItem(key);
  });
  await other.goto(`/search?q=${word}`);
  await expect(other.locator("[data-node-id]").filter({ hasText: `${word}-1.txt` })).toBeVisible();
  await expect(box(other)).toHaveValue(word);
  expect(await focus(other)).toEqual({ tag: "BODY" });
  await other.getByRole("button", { name: "Clear search", exact: true }).click();
  await expect(other).toHaveURL(/\/search$/);
  await expect(other.getByText("Type in the search box to find files")).toBeVisible();
  await expect(other.locator("[data-node-id]")).toHaveCount(0);
  await other.close();
});
