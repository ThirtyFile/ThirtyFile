// How much is used: the status bar shows the space being browsed, and Storage usage counts folder spaces.
import { expect, test } from "@playwright/test";
import { signIn } from "./helpers";

interface Drive {
  kind: string;
  mode: string;
  root_id: string;
  used_bytes: number;
}

test("the status bar shows the space being browsed, and Storage usage counts folder spaces", async ({ page }) => {
  await signIn(page);
  const drives: Drive[] = await (await page.request.get("/api/drives")).json();
  const company = drives.find((d) => d.kind === "company")!;
  // A new installation keeps its spaces in folders on the disk
  expect(company.mode).toBe("folder");
  await page.goto(`/files/${company.root_id}`);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name: `usage ${Date.now().toString(36)}.txt`, mimeType: "text/plain", buffer: Buffer.alloc(3000, 97) }]);
  await expect(page.locator("[data-node-id]").filter({ hasText: "usage " })).toBeVisible();

  // Not the personal space's figure: the one of "All files"
  const used = async () => ((await (await page.request.get("/api/drives")).json()) as Drive[]).find((d) => d.kind === "company")!.used_bytes;
  await expect.poll(used).toBeGreaterThanOrEqual(3000);
  const kb = `${((await used()) / 1024).toFixed(1)} KB used`;
  await page.getByRole("button", { name: "Refresh", exact: true }).first().click();
  await expect(page.locator("footer").getByText(kb)).toBeVisible();

  // The actual disk use includes the folder spaces' files
  const settings = await (await page.request.get("/api/admin/settings")).json();
  expect(settings.stats.stored_bytes).toBeGreaterThanOrEqual(3000);
});

test("Storage usage shows each location, its history as charts and tables, and saves alert thresholds", async ({ page }) => {
  await signIn(page);
  const drives: Drive[] = await (await page.request.get("/api/drives")).json();
  const company = drives.find((d) => d.kind === "company")!;
  await page.goto(`/files/${company.root_id}`);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name: `chart ${Date.now().toString(36)}.txt`, mimeType: "text/plain", buffer: Buffer.alloc(5000, 98) }]);
  await expect(page.locator("[data-node-id]").filter({ hasText: "chart " })).toBeVisible();
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

  // Thresholds are saved, and a disk above one raises an alert at the top of the page
  await page.getByLabel("Alert when a disk is fuller than").fill("0.001");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByRole("heading", { name: /\d+ alerts?/ })).toBeVisible();
  await expect(page.getByText(/Local disk: the disk is .* full/)).toBeVisible();
  // (100: a test machine's disk may well be fuller than the usual 90%)
  await page.getByLabel("Alert when a disk is fuller than").fill("100");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Local disk: the disk is .* full/)).toBeHidden();

  // At phone width nothing makes the page scroll sideways
  await page.setViewportSize({ width: 375, height: 800 });
  const overflow = await page.evaluate(() => [...document.querySelectorAll("figure, article")].some((e) => e.scrollWidth > e.clientWidth + 1));
  expect(overflow).toBe(false);
});
