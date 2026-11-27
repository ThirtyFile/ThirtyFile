// Opening a ZIP file shows what it holds; one file is downloaded from it, and a selection extracted next to it.
import { readFile } from "node:fs/promises";
import { expect, test } from "@playwright/test";
import JSZip from "jszip";
import { answer, makeFolder, openFolder, signIn, uploadFile } from "./helpers";

test("a ZIP file is looked into, one file downloaded from it and a folder of it extracted", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "ZIP browsing");
  const zip = new JSZip();
  zip.file("notes.txt", "outside");
  zip.file("Photos/2026/a.txt", "inside the archive");
  zip.file("Photos/b.txt", "b");
  await uploadFile(page, dir, "pack.zip", await zip.generateAsync({ type: "nodebuffer" }));
  await openFolder(page, dir);
  await page.getByRole("grid").getByRole("row").filter({ hasText: "pack.zip" }).dblclick();

  const table = page.getByRole("table");
  const row = (name: string) => table.getByRole("row").filter({ hasText: name });
  await expect(row("Photos")).toBeVisible();
  await expect(row("notes.txt")).toBeVisible();
  await expect(page.getByRole("button", { name: "Extract selected" })).toBeDisabled();

  await table.getByRole("button", { name: "Photos" }).click();
  await table.getByRole("button", { name: "2026" }).click();
  const downloading = page.waitForEvent("download");
  await table.getByRole("button", { name: "Download a.txt" }).click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("a.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("inside the archive");

  // Back up, by the path above the list
  await page.getByRole("navigation", { name: "Folders in the ZIP file" }).getByRole("button", { name: "pack.zip" }).click();
  await table.getByRole("checkbox", { name: "Select Photos" }).click();
  const started = answer(page, "POST", "/api/archive/extract");
  await page.getByRole("button", { name: "Extract selected" }).click();
  expect((await started).ok()).toBe(true);
  await expect(page.getByText('Extracted to "pack"')).toBeVisible();

  const children = async (folder: string) => (await (await page.request.get(`/api/nodes/${folder}/children`)).json()) as { id: string; name: string }[];
  const extracted = (await children(dir)).find((n) => n.name === "pack")!;
  expect((await children(extracted.id)).map((n) => n.name)).toEqual(["Photos"]);
});
