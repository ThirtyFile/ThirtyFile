import { expect, type Page } from "@playwright/test";
import JSZip from "jszip";

export const office = (page: Page, id?: string) => page.locator(id ? `[data-office-document="${id}"]` : "[data-office-document]").filter({ visible: true });
export async function officeCall(page: Page, method: string, ...args: unknown[]): Promise<any> {
  return office(page)
    .locator("[data-office-session]")
    .evaluate(
      async (element, { method, args }) => {
        const session = (element as any).officeSession;
        return session[method](...args);
      },
      { method, args },
    );
}
export async function readyOffice(page: Page) {
  await expect(office(page)).toHaveAttribute("data-office-ready", "true", { timeout: 120_000 });
  await expect(office(page).locator("canvas")).toBeVisible();
  await expect(office(page).locator("[data-office-session]")).toHaveCount(1);
  await expect(office(page).locator("iframe")).toHaveCount(0);
}
export async function openOffice(page: Page, id: string, kind: "word" | "sheets" | "slides") {
  await page.goto(`/view/${id}`);
  await page.getByRole("button", { name: kind === "word" ? "Edit document" : kind === "sheets" ? "Edit workbook" : "Edit presentation", exact: true }).click();
  await expect(office(page).getByRole("button", { name: "Save", exact: true })).toBeVisible();
  await readyOffice(page);
  if (kind === "word") await officeCall(page, "execute", "file.autosave", { value: false });
}
export async function officeFile(page: Page, id: string) {
  const response = await page.request.get(`/api/files/${id}/content`);
  expect(response.ok()).toBe(true);
  return JSZip.loadAsync(await response.body());
}

export const sheetGo = (page: Page, range: string) => officeCall(page, "execute", "selection.set", { range });
export const sheetRead = async (page: Page, range: string) => (await officeCall(page, "execute", "sheet.read", { range, formatted: true })).values;
