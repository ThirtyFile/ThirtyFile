// Finder density, window chrome and command overflow against the embedded production interface.
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";
import { answer, makeFolder, openFolder, signInAsNewUser, unique, uploadFile } from "./helpers";

async function setUp(page: Page, theme = "light") {
  await page.setViewportSize({ width: 1280, height: 720 });
  await page.addInitScript((theme) => {
    localStorage.setItem("tf-theme", theme);
    localStorage.setItem("tf-view", JSON.stringify("list"));
  }, theme);
  await signInAsNewUser(page, "finder");
  expect((await page.request.put("/api/auth/style", { data: { style: "mac" } })).ok()).toBe(true);
  return makeFolder(page, "Finder");
}
const rows = (page: Page) => page.locator("table[aria-multiselectable] [data-node-id]");
const item = (page: Page, name: string) => rows(page).filter({ has: page.locator("[data-name]", { hasText: new RegExp(`^${name}$`) }) });
async function viewOptions(page: Page) {
  const wide = page.getByRole("button", { name: "View options", exact: true });
  await ((await wide.count()) ? wide : page.getByRole("button", { name: "View", exact: true })).click();
}

for (const theme of ["light", "dark"])
  test(`compact rows, thumbnails, sorting and selection pass axe (${theme})`, async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    const folder = await setUp(page, theme);
    const photo = await uploadFile(page, folder, "photo.png", Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=", "base64"));
    await uploadFile(page, folder, "readme.txt", "Example text");
    await openFolder(page, folder);
    await expect(rows(page)).toHaveCount(2);
    await expect
      .poll(() =>
        rows(page)
          .first()
          .evaluate((r) => r.getBoundingClientRect().height),
      )
      .toBe(19);
    await expect(page.getByRole("columnheader")).toHaveText(["Name", "Date modified", "Size", "Kind"]);
    await expect(item(page, "photo.png").locator("img")).toBeVisible();
    await expect(item(page, "photo.png").locator("img")).toHaveAttribute("src", new RegExp(photo));
    const stripe = await rows(page).evaluateAll((rs) => rs.map((r) => getComputedStyle(r.querySelector("td")!).backgroundColor));
    expect(stripe[0]).not.toBe(stripe[1]);
    await item(page, "readme.txt").click();
    await expect(item(page, "readme.txt")).toHaveAttribute("aria-selected", "true");
    await page.getByRole("columnheader", { name: "Name" }).getByRole("button", { name: "Name" }).click();
    const sorted = page.getByRole("columnheader", { name: "Name" });
    await expect(sorted).toHaveAttribute("aria-sort", "descending");
    expect(await sorted.locator("button svg").count()).toBe(1);
    const violations = (await new AxeBuilder({ page }).analyze()).violations.map((v) => ({ id: v.id, nodes: v.nodes.map((n) => n.target) }));
    expect(violations).toEqual([]);
    expect(errors).toEqual([]);
  });

test("bars are opt-in, persist after reload, and a single tab hides until another opens", async ({ page }) => {
  const folder = await setUp(page);
  const inner = await makeFolder(page, "Inner", folder);
  await openFolder(page, folder);
  await expect(page.getByRole("navigation", { name: "Path bar" })).toHaveCount(0);
  await expect(page.locator("[data-mac-status]")).toHaveCount(0);
  await expect(page.getByRole("tablist")).toHaveCount(0);
  await expect(page.getByRole("banner")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Notifications/ })).toHaveCount(1);
  for (const label of ["Show path bar", "Show status bar"]) {
    await viewOptions(page);
    await page.getByRole("menuitemcheckbox", { name: label }).click();
  }
  await expect(page.getByRole("navigation", { name: "Path bar" })).toBeVisible();
  await expect(page.locator("[data-mac-status]")).toContainText("1 item");
  await page.reload();
  await expect(page.getByRole("navigation", { name: "Path bar" })).toBeVisible();
  await expect(page.locator("[data-mac-status]")).toBeVisible();
  await item(page, "Inner").click({ button: "middle" });
  await expect(page.getByRole("tab")).toHaveCount(2);
  await expect(page.getByRole("banner")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Notifications/ })).toHaveCount(1);
  await page.getByRole("tab").last().click();
  await page.waitForURL(`**/files/${inner}`);
  await page.keyboard.press("Delete");
  await expect(page.getByRole("tablist")).toHaveCount(0);
  await expect(page.getByRole("banner")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Notifications/ })).toHaveCount(1);
  await expect(page.locator("#tf-main")).toBeFocused();
});

test("toolbar stays on one line when resized and optional commands remain in Actions", async ({ page }) => {
  const folder = await setUp(page);
  await uploadFile(page, folder, "notes.txt", "notes");
  await openFolder(page, folder);
  await item(page, "notes.txt").click();
  await expect(page.locator("[data-mac-toolbar]").getByRole("button", { name: "Tags", exact: true })).toBeVisible();
  await page.setViewportSize({ width: 900, height: 720 });
  await expect(page.locator("[data-mac-toolbar]")).toHaveAttribute("data-compact", "true");
  await expect(page.locator("[data-mac-toolbar]").getByRole("button", { name: "Tags", exact: true })).toHaveCount(0);
  for (const width of [1100, 1000, 950, 900, 800, 768]) {
    await page.setViewportSize({ width, height: 720 });
    await expect
      .poll(async () => {
        const geometry = await page.locator("[data-mac-toolbar]").evaluate((el) => ({ height: el.clientHeight, width: el.clientWidth, scroll: el.scrollWidth }));
        return geometry.height === 51 && geometry.scroll <= geometry.width;
      })
      .toBe(true);
  }
  await page.getByRole("button", { name: "Actions", exact: true }).click();
  for (const name of ["Tags", "Sort by", "Group by", "Share"]) await expect(page.getByRole("menuitem", { name, exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await viewOptions(page);
  await page.getByRole("menuitemcheckbox", { name: "Gallery", exact: true }).click();
  await page.getByRole("button", { name: "Actions", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: "Group by", exact: true })).toBeDisabled();
  await page.keyboard.press("Escape");
  await viewOptions(page);
  await page.getByRole("menuitemcheckbox", { name: "List", exact: true }).click();
  await expect(rows(page)).toHaveCount(1);
  await page.setViewportSize({ width: 1280, height: 720 });
  await expect(page.locator("[data-mac-toolbar]").getByRole("button", { name: "Tags", exact: true })).toBeVisible();
  // Resizing the sidebar also reduces the actual room, without changing viewport width.
  await page.setViewportSize({ width: 1100, height: 720 });
  await expect(page.locator("[data-mac-toolbar]")).not.toHaveAttribute("data-compact", "true");
  const handle = page.getByRole("separator", { name: "Resize navigation pane" });
  await handle.focus();
  for (let i = 0; i < 12; i++) await handle.press("ArrowRight");
  await expect(page.locator("[data-mac-toolbar]")).toHaveAttribute("data-compact", "true");
});

test("Tags in the toolbar uses the same assignment choices as the context menu", async ({ page }) => {
  const folder = await setUp(page);
  await uploadFile(page, folder, "notes.txt", "notes");
  const name = `Review ${unique()}`;
  expect((await page.request.post("/api/tags", { data: { name, color: "red" } })).ok()).toBe(true);
  await openFolder(page, folder);
  await item(page, "notes.txt").click();
  await page.locator("[data-mac-toolbar]").getByRole("button", { name: "Tags", exact: true }).click();
  const changed = answer(page, "POST", "/api/nodes/tags");
  await page.getByRole("menuitemcheckbox", { name, exact: true }).click();
  await changed;
  await page.locator("[data-mac-toolbar]").getByRole("button", { name: "Tags", exact: true }).click();
  await expect(page.getByRole("menuitemcheckbox", { name, exact: true })).toHaveAttribute("aria-checked", "true");
});

test("a large Finder list keeps 19 px geometry and absolute stripes through unloaded windows and keyboard paging", async ({ page }) => {
  test.setTimeout(150_000);
  const folder = await setUp(page);
  const name = (i: number) => `Item ${String(i).padStart(4, "0")}`;
  for (let i = 0; i < 1200; i += 25) await Promise.all(Array.from({ length: Math.min(25, 1200 - i) }, (_, k) => makeFolder(page, name(i + k), folder)));
  const offsets: number[] = [];
  page.on("request", (r) => {
    const u = new URL(r.url());
    if (u.pathname === `/api/nodes/${folder}/children` && u.searchParams.has("offset")) offsets.push(Number(u.searchParams.get("offset")));
  });
  await openFolder(page, folder);
  await expect(item(page, name(0))).toBeVisible();
  expect(offsets).not.toContain(1000);
  expect(await rows(page).count()).toBeLessThan(100);
  await item(page, name(0)).click();
  const step = await item(page, name(0)).evaluate((el) => {
    let scroll: HTMLElement | null = el.parentElement;
    while (scroll && !["auto", "scroll"].includes(getComputedStyle(scroll).overflowY)) scroll = scroll.parentElement;
    return Math.max(1, Math.floor(((scroll?.clientHeight ?? innerHeight) - 30) / 19) - 1);
  });
  await page.keyboard.press("PageDown");
  await expect(item(page, name(step))).toBeFocused();
  await page.keyboard.press("End");
  await expect(item(page, name(1199))).toBeFocused();
  expect(offsets).toContain(1000);
  await expect.poll(() => item(page, name(1199)).evaluate((r) => r.getBoundingClientRect().height)).toBe(19);
  const mismatches = await rows(page).evaluateAll((rs) => rs.filter((r) => r.getAttribute("data-stripe") !== (Number(r.getAttribute("aria-rowindex")) % 2 ? "odd" : "even")).length);
  expect(mismatches).toBe(0);
  await page.keyboard.press("Home");
  await expect(item(page, name(0))).toBeFocused();
});

test("phone and Windows layouts keep their existing list density and frame", async ({ page }) => {
  const folder = await setUp(page);
  await uploadFile(page, folder, "notes.txt", "notes");
  await page.setViewportSize({ width: 375, height: 812 });
  await openFolder(page, folder);
  await expect(page.getByRole("button", { name: "Navigation pane" })).toBeVisible();
  await expect
    .poll(() =>
      rows(page)
        .first()
        .evaluate((r) => r.getBoundingClientRect().height),
    )
    .toBe(28);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(375);
  await page.setViewportSize({ width: 1280, height: 720 });
  expect((await page.request.put("/api/auth/style", { data: { style: "windows" } })).ok()).toBe(true);
  await page.reload();
  await expect(page.getByRole("navigation", { name: "File path" })).toBeVisible();
  await expect(page.getByRole("tablist")).toBeVisible();
  await expect
    .poll(() =>
      rows(page)
        .first()
        .evaluate((r) => r.getBoundingClientRect().height),
    )
    .toBe(28);
  await expect(page.getByRole("columnheader")).toHaveText(["Name", "Date modified", "Type", "Size"]);
});
