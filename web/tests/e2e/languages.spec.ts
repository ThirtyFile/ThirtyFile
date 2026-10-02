// The interface's languages: the switch, <html lang>, the browser's language for people who aren't signed in, and the
// choice kept after signing in again. Expected text comes from the dictionaries (no translations in the tests).
import { expect, test, type Page } from "@playwright/test";
import { DICT as ZH_TW } from "../../src/lib/i18n/zh-TW";
import { DICT as JA } from "../../src/lib/i18n/ja";
import { PASSWORD, signIn } from "./helpers";

/** Offers the languages that aren't ready yet, as `localStorage["tf-lang-preview"]` does */
const preview = (page: Page) => page.addInitScript(() => localStorage.setItem("tf-lang-preview", "1"));

const htmlLang = (page: Page) => page.locator("html").getAttribute("lang");

/** The language switch of the sign-in page */
const languageSwitch = (page: Page) => page.getByRole("combobox");

test("the sign-in page offers the ready languages and switches between them", async ({ page }) => {
  await page.goto("/login");
  await expect(languageSwitch(page)).toHaveValue("en");
  expect(await htmlLang(page)).toBe("en");
  // Simplified Chinese and Japanese stay hidden until they are translated
  expect(
    await languageSwitch(page)
      .locator("option")
      .evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value)),
  ).toEqual(["en", "zh-TW"]);

  await languageSwitch(page).selectOption("zh-TW");
  await expect(page.getByRole("button", { name: ZH_TW["Click or press any key to sign in"] })).toBeVisible();
  expect(await htmlLang(page)).toBe("zh-Hant");
  // The server puts the chosen language's dictionary on the page
  await expect(page.locator('script[src="/zh-TW.js"]')).toHaveCount(1);

  await languageSwitch(page).selectOption("en");
  await expect(page.getByRole("button", { name: "Click or press any key to sign in" })).toBeVisible();
  expect(await htmlLang(page)).toBe("en");
  await expect(page.locator('script[src$=".js"][src^="/"]:not([src^="/assets/"])')).toHaveCount(0);
});

test("with the preview flag, each of the four languages can be chosen, with its own <html lang> and dictionary", async ({ page }) => {
  await preview(page);
  await page.goto("/login");
  await expect(languageSwitch(page).locator("option")).toHaveCount(4);
  expect(
    await languageSwitch(page)
      .locator("option")
      .evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value)),
  ).toEqual(["en", "zh-TW", "zh-CN", "ja"]);
  for (const [lang, tag] of [
    ["zh-CN", "zh-Hans"],
    ["ja", "ja"],
    ["zh-TW", "zh-Hant"],
    ["en", "en"],
  ]) {
    await languageSwitch(page).selectOption(lang);
    await expect(page.locator("html")).toHaveAttribute("lang", tag);
    await expect(languageSwitch(page)).toHaveValue(lang);
    if (lang !== "en") await expect(page.locator(`script[src="/${lang}.js"]`)).toHaveCount(1);
  }
});

test("a visitor whose browser is Japanese gets English while Japanese isn't ready, and Japanese once it is", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ baseURL, locale: "ja-JP" });
  const page = await context.newPage();
  await page.goto("/login");
  await expect(page.getByRole("button", { name: "Click or press any key to sign in" })).toBeVisible();
  expect(await htmlLang(page)).toBe("en");
  await expect(languageSwitch(page)).toHaveValue("en");

  await preview(page);
  await page.reload();
  await expect(page.getByRole("button", { name: JA["Click or press any key to sign in"] })).toBeVisible();
  await expect(languageSwitch(page)).toHaveValue("ja");
  expect(await htmlLang(page)).toBe("ja");
  await context.close();
});

test("a visitor whose browser is Simplified Chinese gets Traditional Chinese while Simplified Chinese isn't ready", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ baseURL, locale: "zh-CN" });
  const page = await context.newPage();
  await page.goto("/login");
  await expect(page.getByRole("button", { name: ZH_TW["Click or press any key to sign in"] })).toBeVisible();
  expect(await htmlLang(page)).toBe("zh-Hant");
  await context.close();
});

test("the language chosen in the account menu is kept after signing out and in again", async ({ page }) => {
  await signIn(page);
  await page.getByRole("button", { name: /^a\s*admin$/i }).click();
  await page.getByRole("menuitem", { name: /Language/ }).click();
  // The languages in the order of the switch: English, then Traditional Chinese
  const choices = page.getByRole("menuitemradio");
  await expect(choices).toHaveCount(2);
  await choices.nth(1).click();
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");

  await page.getByRole("button", { name: /^a\s*admin$/i }).click();
  await page.getByRole("menuitem", { name: ZH_TW["Sign out"] }).click();
  await page.waitForURL(/\/login/);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");

  await page.getByRole("button", { name: ZH_TW["Click or press any key to sign in"] }).click();
  const username = page.getByLabel(ZH_TW["Username"]);
  await expect(username).toBeFocused();
  await username.fill("admin");
  await page.getByLabel(ZH_TW["Password"]).fill(PASSWORD);
  await page.getByRole("button", { name: ZH_TW["Sign in"], exact: true }).click();
  await page.waitForURL(/\/files/);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");
  await expect(page.locator('script[src="/zh-TW.js"]')).toHaveCount(1);
});
