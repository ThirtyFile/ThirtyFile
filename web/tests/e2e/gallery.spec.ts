// The Mac style's Gallery view (components/style/mac/gallery.tsx): a large preview of the item selected above a strip of
// thumbnails. Moving through pictures, a video and a document with the keys, Quick look from it, a large folder loaded a
// part at a time across the strip, and file operations from the view. Each test signs in as an account of its own, so
// the other tests keep the Windows style.
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, openFolder, signInAsNewUser, uploadFile } from "./helpers";

/** Signed in as a new account that chose the Mac style and the Gallery view, at a computer's width */
async function gallery(page: Page) {
  await page.setViewportSize({ width: 1280, height: 720 });
  await signInAsNewUser(page, "gallery");
  expect((await page.request.put("/api/auth/style", { data: { style: "mac" } })).ok()).toBe(true);
  await page.addInitScript(() => localStorage.setItem("tf-view", JSON.stringify("gallery")));
}

/** A picture of one pixel */
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=", "base64");

const strip = (page: Page) => page.getByRole("listbox", { name: "Thumbnails" });
const thumb = (page: Page, name: string) => strip(page).getByRole("option", { name, exact: true });
const preview = (page: Page, name: string) => page.getByRole("region", { name: `Preview: ${name}` });
const announced = (page: Page, text: string) => page.getByRole("status").filter({ hasText: text });
const childNames = async (page: Page, id: string): Promise<string[]> =>
  ((await (await page.request.get(`/api/nodes/${id}/children`)).json()) as { name: string }[]).map((n) => n.name).sort();

test("→ and ← move through a folder of pictures, a video and a document, each previewed and announced; Space is Quick look", async ({ page }) => {
  await gallery(page);
  const top = await makeFolder(page, "Gallery");
  await makeFolder(page, "Album", top);
  await uploadFile(page, top, "a-beach.png", PNG);
  await uploadFile(page, top, "b-clip.webm", "not really a video");
  await uploadFile(page, top, "c-notes.txt", "the notes' own words");
  await openFolder(page, top);

  // The view chosen in the toolbar; the first item shows from the start
  await expect(page.getByRole("group", { name: "View" }).getByRole("button", { name: "Gallery" })).toHaveAttribute("aria-pressed", "true");
  await expect(thumb(page, "Album")).toHaveAttribute("aria-selected", "true");
  await expect(preview(page, "Album")).toContainText("File folder");
  await expect(preview(page, "Album")).toContainText("1 of 4");

  await thumb(page, "Album").focus();
  await page.keyboard.press("ArrowRight");
  await expect(thumb(page, "a-beach.png")).toBeFocused();
  await expect(thumb(page, "a-beach.png")).toHaveAttribute("aria-selected", "true");
  await expect(announced(page, "a-beach.png, 2 of 4")).toBeAttached();
  await expect(preview(page, "a-beach.png").getByRole("img", { name: "a-beach.png" })).toBeVisible();

  await page.keyboard.press("ArrowRight");
  await expect(announced(page, "b-clip.webm, 3 of 4")).toBeAttached();
  // The player, without playing; or, as this isn't a real video, what is shown when the browser can't play it
  const player = preview(page, "b-clip.webm").locator("video");
  await expect(player.or(preview(page, "b-clip.webm").getByText("Your browser can't show this file"))).toBeVisible();
  if (await player.count()) expect(await player.evaluate((v: HTMLVideoElement) => v.autoplay)).toBe(false);

  await page.keyboard.press("ArrowRight");
  await expect(announced(page, "c-notes.txt, 4 of 4")).toBeAttached();
  await expect(preview(page, "c-notes.txt")).toContainText("the notes' own words");
  // The details beside it
  await expect(page.getByRole("complementary", { name: "Details" })).toContainText("c-notes.txt");

  // Space: Quick look on the item, and back to the strip
  await page.keyboard.press("Space");
  await expect(page.getByRole("dialog", { name: "Quick look: c-notes.txt" })).toBeVisible();
  await page.keyboard.press("Space");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(thumb(page, "c-notes.txt")).toBeFocused();

  await page.keyboard.press("ArrowLeft");
  await expect(thumb(page, "b-clip.webm")).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Home");
  await expect(preview(page, "Album")).toBeVisible();

  // The details panel can be hidden, and stays hidden
  await page.getByRole("button", { name: "Hide details" }).click();
  await expect(page.getByRole("complementary", { name: "Details" })).toHaveCount(0);
  // ...which leaves the item selected (a click on the list's empty space would clear it)
  await expect(thumb(page, "Album")).toHaveAttribute("aria-selected", "true");
  await page.reload();
  await expect(thumb(page, "Album")).toBeVisible();
  await expect(page.getByRole("button", { name: "Show details" })).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Details" })).toHaveCount(0);
});

/** More than two parts of 500: the part at the end isn't loaded when the folder opens */
const COUNT = 1100;
const itemName = (i: number) => `Item ${String(i).padStart(4, "0")}`;

test("a large folder loads a part at a time across the strip; End reaches the last item and previews it", async ({ page }) => {
  test.setTimeout(120_000);
  await gallery(page);
  const big = await makeFolder(page, "Large gallery");
  for (let i = 0; i < COUNT; i += 25) await Promise.all(Array.from({ length: Math.min(25, COUNT - i) }, (_, k) => makeFolder(page, itemName(i + k), big)));
  const offsets: number[] = [];
  page.on("request", (r) => {
    const url = new URL(r.url());
    if (url.pathname === `/api/nodes/${big}/children` && url.searchParams.has("offset")) offsets.push(Number(url.searchParams.get("offset")));
  });
  await openFolder(page, big);
  await expect(page.locator("footer")).toContainText(`${COUNT.toLocaleString("en-US")} items`);
  await expect(thumb(page, itemName(0))).toHaveAttribute("aria-selected", "true");
  expect(offsets).not.toContain(1000);

  const last = page.waitForResponse((r) => {
    const url = new URL(r.url());
    return url.pathname === `/api/nodes/${big}/children` && url.searchParams.get("offset") === "1000";
  });
  await thumb(page, itemName(0)).focus();
  await page.keyboard.press("End");
  await last;
  await expect(thumb(page, itemName(COUNT - 1))).toHaveAttribute("aria-selected", "true");
  await expect(thumb(page, itemName(COUNT - 1))).toBeFocused();
  expect(offsets).toContain(1000);
  await expect(preview(page, itemName(COUNT - 1))).toBeVisible();
  await expect(announced(page, `${itemName(COUNT - 1)}, ${COUNT.toLocaleString("en-US")} of ${COUNT.toLocaleString("en-US")}`)).toBeAttached();
  // Only the thumbnails near those in view are in the page
  expect(await strip(page).getByRole("option").count()).toBeLessThan(100);

  await page.keyboard.press("ArrowLeft");
  await expect(thumb(page, itemName(COUNT - 2))).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Home");
  await expect(thumb(page, itemName(0))).toBeFocused();
  await expect(preview(page, itemName(0))).toBeVisible();
});

test("from the view: Enter renames, the details panel moves to the trash, and a thumbnail dragged onto a folder moves there", async ({ page }) => {
  await gallery(page);
  const top = await makeFolder(page, "Gallery ops");
  const archive = await makeFolder(page, "Archive", top);
  await uploadFile(page, top, "one.txt", "1");
  await uploadFile(page, top, "two.txt", "2");
  await openFolder(page, top);

  // Enter: the name above the preview turns into a box; the strip has the focus again after
  await thumb(page, "one.txt").click();
  await expect(preview(page, "one.txt")).toBeVisible();
  await page.keyboard.press("Enter");
  const box = page.getByRole("textbox", { name: "New name" });
  await expect(box).toBeFocused();
  await box.fill("uno.txt");
  const renamed = answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);
  await box.press("Enter");
  expect((await renamed).ok()).toBe(true);
  await expect(thumb(page, "uno.txt")).toBeFocused();
  await expect(preview(page, "uno.txt")).toBeVisible();

  // The details panel's Move to trash, after the question every style asks
  await thumb(page, "two.txt").click();
  await page.getByRole("complementary", { name: "Details" }).getByRole("button", { name: "Move to trash" }).click();
  const ask = page.getByRole("alertdialog").or(page.getByRole("dialog")).filter({ hasText: "Move 1 item to trash?" });
  const trashed = answer(page, "POST", "/api/nodes/trash");
  await ask.getByRole("button", { name: "Move to trash" }).click();
  expect((await trashed).ok()).toBe(true);
  await expect(thumb(page, "two.txt")).toHaveCount(0);

  // Dragged onto a folder of the strip: moved into it
  const moved = answer(page, "POST", "/api/nodes/move");
  await thumb(page, "uno.txt").dragTo(thumb(page, "Archive"));
  expect((await moved).ok()).toBe(true);
  await expect(thumb(page, "uno.txt")).toHaveCount(0);
  expect(await childNames(page, archive)).toEqual(["uno.txt"]);
});
