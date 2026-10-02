// How much is used: the status bar shows the space being browsed, and Storage usage counts folder spaces.
import { expect, test, type Page } from "@playwright/test";
import { makeFolder, openFolder, signIn, unique, uploadFinished } from "./helpers";

interface Drive {
  kind: string;
  mode: string;
  root_id: string;
  used_bytes: number;
}

// An upload is a few changes on the server, and saving thresholds one more: behind a large change made by another test
// at the same time, each of them can wait half a minute for its turn
test.describe.configure({ timeout: 120_000 });

/** A size the way the page writes it (formatBytes) */
function size(n: number) {
  if (n < 1024) return `${n} B`;
  let v = n / 1024;
  let unit = 0;
  while (v >= 1024 && unit < 3) [v, unit] = [v / 1024, unit + 1];
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${["KB", "MB", "GB", "TB"][unit]}`;
}

/** The "All files" space: other tests (and other copies of these) use it at the same time */
async function companySpace(page: Page): Promise<Drive> {
  const drives: Drive[] = await (await page.request.get("/api/drives")).json();
  return drives.find((d) => d.kind === "company")!;
}

/**
 * Uploads a file of `bytes` bytes through the page into a new folder of the space, and waits until the server has saved
 * it. The folder is this test's own (a random name), so its file is in view however many the space has.
 */
async function upload(page: Page, space: Drive, name: string, bytes: number) {
  await openFolder(page, await makeFolder(page, `Usage ${unique()}`, space.root_id));
  const finished = uploadFinished(page);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name, mimeType: "text/plain", buffer: Buffer.alloc(bytes, 97) }]);
  await finished;
  await expect(page.locator("[data-node-id]").filter({ hasText: name })).toBeVisible();
}

test("the status bar shows the space being browsed, and Storage usage counts folder spaces", async ({ page }) => {
  await signIn(page);
  const company = await companySpace(page);
  // A new installation keeps its spaces in folders on the disk
  expect(company.mode).toBe("folder");
  await upload(page, company, "usage.txt", 3000);

  // Not the personal space's figure: the one of "All files", which counts the file once the server has saved it
  const used = async () => (await companySpace(page)).used_bytes;
  expect(await used()).toBeGreaterThanOrEqual(company.used_bytes + 3000);
  await page.getByRole("button", { name: "Refresh", exact: true }).first().click();
  // (read again each time: other tests may upload into the same space meanwhile)
  await expect.poll(async () => (await page.locator("footer").textContent())?.includes(`${size(await used())} used`)).toBe(true);

  // The actual disk use includes the folder spaces' files
  const settings = await (await page.request.get("/api/admin/settings")).json();
  expect(settings.stats.stored_bytes).toBeGreaterThanOrEqual(3000);
});

test("Storage usage shows each location, its history as charts and tables, and saves alert thresholds", async ({ page }) => {
  await signIn(page);
  await upload(page, await companySpace(page), "chart.txt", 5000);
  // The first capacity sample is taken shortly after the start
  await expect.poll(async () => (await (await page.request.get("/api/admin/usage")).json()).total !== null, { timeout: 30_000 }).toBe(true);

  await page.goto("/admin/usage");
  const local = page.getByRole("article", { name: "Local disk" });
  await expect(local.getByText("Online")).toBeVisible();
  // The upload just made is counted, before any sample of operations is written
  await expect(local.getByText(/Writes: \d+ operation/)).toBeVisible();
  await expect(local.getByRole("meter", { name: "Disk used" })).toBeVisible();

  // Capacity has a chart with a text alternative and a table; operations not written yet say so rather than showing 0
  const content = page.getByRole("figure").filter({ hasText: "The last sample of each" }).first();
  await expect(content.getByRole("img")).toHaveAttribute("aria-label", /Content: Files from/);
  await content.getByText("Show as a table").click();
  await expect(content.getByRole("table")).toBeVisible();
  await expect(content.getByRole("columnheader", { name: "Stored by ThirtyFile" })).toBeVisible();
  // Operations are written at every five-minute mark of the clock: before the first, their chart says there are none
  // rather than showing 0, after it the chart shows them
  const throughput = page.getByRole("figure").filter({ hasText: "Throughput" });
  await expect(throughput.getByText("No reads or writes in this range").or(throughput.getByRole("img"))).toBeVisible();
  // Moving along a chart with the keyboard shows the values of a period
  await content.getByRole("img").focus();
  await page.keyboard.press("ArrowRight");
  await expect(content.getByRole("status")).toContainText("Files:");

  await page.getByLabel("Time range").selectOption("30d");
  await expect(page.getByText(/the last sample of each hour/i).first()).toBeVisible();

  // Thresholds are saved, and a disk above one raises an alert at the top of the page. Saving waits its turn behind
  // other changes on the server: the page shows the alerts once the server has answered.
  const save = async (percent: string) => {
    await page.getByLabel("Alert when a disk is fuller than").fill(percent);
    const saved = page.waitForResponse((r) => r.request().method() === "PUT" && new URL(r.url()).pathname === "/api/admin/usage/thresholds");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    expect((await saved).ok()).toBe(true);
  };
  await save("0.001");
  await expect(page.getByRole("heading", { name: /\d+ alerts?/ })).toBeVisible();
  await expect(page.getByText(/Local disk: the disk is .* full/)).toBeVisible();
  // (100: a test machine's disk may well be fuller than the usual 90%)
  await save("100");
  await expect(page.getByText(/Local disk: the disk is .* full/)).toBeHidden();

  // At phone width nothing makes the page scroll sideways
  await page.setViewportSize({ width: 375, height: 800 });
  const overflow = await page.evaluate(() => [...document.querySelectorAll("figure, article")].some((e) => e.scrollWidth > e.clientWidth + 1));
  expect(overflow).toBe(false);
});
