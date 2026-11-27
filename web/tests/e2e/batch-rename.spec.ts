// Renaming several items at once: a preview of every new name, one change for all of them, and Undo.
import { expect, test } from "@playwright/test";
import { answer, makeFolder, openFolder, signIn, uploadFile } from "./helpers";

test("several files are numbered together after a preview, and Undo gives them their names back", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Batch rename");
  for (const name of ["a.txt", "b.txt", "kept.txt"]) await uploadFile(page, dir, name, name);
  await openFolder(page, dir);
  const grid = page.getByRole("grid");
  const row = (name: string) => grid.getByRole("row").filter({ hasText: name });
  await row("a.txt").click();
  await row("b.txt").click({ modifiers: ["ControlOrMeta"] });
  await page.keyboard.press("F2");

  const dialog = page.getByRole("dialog", { name: "Rename 2 items" });
  const preview = dialog.getByRole("list", { name: "Preview" });
  const submit = dialog.getByRole("button", { name: "Rename", exact: true });
  await expect(preview.getByRole("listitem")).toHaveCount(2);
  await expect(submit).toBeDisabled();
  // The name of a file that keeps it is marked, and nothing can be renamed until it is fixed
  await expect(dialog.getByLabel("Find")).toBeFocused();
  await page.keyboard.type("a");
  await dialog.getByLabel("Replace with").fill("kept");
  await expect(preview.getByText("Another item here has this name")).toBeVisible();
  await expect(submit).toBeDisabled();

  await dialog.getByLabel("Method").selectOption({ label: "Number the items" });
  await dialog.getByLabel("Base name").fill("page ");
  await dialog.getByLabel("Digits").fill("2");
  await expect(preview.getByRole("listitem").first()).toContainText("page 01.txt");
  await expect(preview.getByRole("listitem").last()).toContainText("page 02.txt");
  const renamed = answer(page, "POST", "/api/nodes/rename");
  await submit.click();
  expect((await renamed).ok()).toBe(true);
  await expect(dialog).toBeHidden();
  await expect(row("page 01.txt")).toBeVisible();
  await expect(row("page 02.txt")).toBeVisible();
  await expect(row("a.txt")).toBeHidden();

  const undone = answer(page, "POST", "/api/nodes/rename");
  await page.getByRole("button", { name: "Undo", exact: true }).click();
  expect((await undone).ok()).toBe(true);
  await expect(row("a.txt")).toBeVisible();
  await expect(row("b.txt")).toBeVisible();
});
