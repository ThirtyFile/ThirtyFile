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
