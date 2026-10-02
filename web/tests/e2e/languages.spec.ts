// The interface's languages: the switch, <html lang>, the browser's language for people who aren't signed in, and the
// choice saved with the account, kept after signing in again and on another browser. Expected text comes from the
// dictionaries (no translations in the tests).
import { expect, test, type Browser, type Page } from "@playwright/test";
import { DICT as ZH_TW } from "../../src/lib/i18n/zh-TW";
import { DICT as JA } from "../../src/lib/i18n/ja";
import { DICT as ZH_CN } from "../../src/lib/i18n/zh-CN";
import { answer, makeUser, signIn, USER_PASSWORD } from "./helpers";

const htmlLang = (page: Page) => page.locator("html").getAttribute("lang");

/** The language switch of the sign-in page */
const languageSwitch = (page: Page) => page.getByRole("combobox");

test("the sign-in page offers every language and switches between them", async ({ page }) => {
  await page.goto("/login");
  await expect(languageSwitch(page)).toHaveValue("en");
  expect(await htmlLang(page)).toBe("en");
  expect(
    await languageSwitch(page)
      .locator("option")
      .evaluateAll((os) => os.map((o) => (o as HTMLOptionElement).value)),
  ).toEqual(["en", "zh-TW", "zh-CN", "ja"]);

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

test("each of the four languages can be chosen, with its own <html lang> and dictionary", async ({ page }) => {
  await page.goto("/login");
  await expect(languageSwitch(page).locator("option")).toHaveCount(4);
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

test("a visitor gets the language of the browser: Japanese, Simplified Chinese or Traditional Chinese", async ({ browser, baseURL }) => {
  for (const [locale, lang, tag, dict] of [
    ["ja-JP", "ja", "ja", JA],
    ["zh-CN", "zh-CN", "zh-Hans", ZH_CN],
    ["zh-TW", "zh-TW", "zh-Hant", ZH_TW],
  ] as const) {
    const context = await browser.newContext({ baseURL, locale });
    const page = await context.newPage();
    await page.goto("/login");
    await expect(page.getByRole("button", { name: dict["Click or press any key to sign in"] })).toBeVisible();
    await expect(languageSwitch(page)).toHaveValue(lang);
    expect(await htmlLang(page)).toBe(tag);
    // The server already put that language's dictionary on the page
    await expect(page.locator(`script[src="/${lang}.js"]`)).toHaveCount(1);
    await context.close();
  }
});

/**
 * A user of their own, so the language saved with the account doesn't reach the other tests (they sign in as the
 * administrator, in parallel). The password an administrator chose is changed first, as the person would
 */
async function newUser(browser: Browser, baseURL: string | undefined): Promise<{ username: string; password: string }> {
  const admin = await browser.newContext({ baseURL });
  const page = await admin.newPage();
  await signIn(page);
  const username = await makeUser(page, "lang");
  const chosen = USER_PASSWORD;
  const password = "another-long-test-password-2";
  await admin.close();
  const own = await browser.newContext({ baseURL });
  expect((await own.request.post("/api/auth/login", { data: { username, password: chosen } })).ok()).toBe(true);
  expect((await own.request.put("/api/auth/password", { data: { current: chosen, new: password } })).ok()).toBe(true);
  await own.close();
  return { username, password };
}

/** Signs in on the sign-in page, in the language it shows */
async function signInAs(page: Page, who: { username: string; password: string }, dict: Record<string, string> = {}) {
  const say = (en: string) => dict[en] ?? en;
  await page.getByRole("button", { name: say("Click or press any key to sign in") }).click();
  const username = page.getByLabel(say("Username"));
  await expect(username).toBeFocused();
  await username.fill(who.username);
  await page.getByLabel(say("Password")).fill(who.password);
  await page.getByRole("button", { name: say("Sign in"), exact: true }).click();
  await page.waitForURL(/\/files/);
}

test("the language chosen in the account menu is kept after signing out and in again", async ({ browser, baseURL, page }) => {
  const who = await newUser(browser, baseURL);
  await page.goto("/login");
  await signInAs(page, who);
  const menu = page.getByRole("button", { name: new RegExp(`${who.username}$`) });
  await menu.click();
  await page.getByRole("menuitem", { name: /Language/ }).click();
  // The languages in the order of the switch: English, then Traditional Chinese
  const choices = page.getByRole("menuitemradio");
  await expect(choices).toHaveCount(4);
  // Saved with the account first, then the page loads again in it: saving may wait its turn behind other tests' changes
  const saved = answer(page, "PUT", "/api/auth/language");
  await choices.nth(1).click();
  expect((await saved).ok()).toBe(true);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");

  await menu.click();
  await page.getByRole("menuitem", { name: ZH_TW["Sign out"] }).click();
  await page.waitForURL(/\/login/);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");

  await signInAs(page, who, ZH_TW);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");
  await expect(page.locator('script[src="/zh-TW.js"]')).toHaveCount(1);
});

test("the language saved with the account follows the person to another browser", async ({ browser, baseURL }) => {
  const who = await newUser(browser, baseURL);
  const first = await browser.newContext({ baseURL });
  const page = await first.newPage();
  await page.goto("/login");
  await signInAs(page, who);
  await page.getByRole("button", { name: new RegExp(`${who.username}$`) }).click();
  await page.getByRole("menuitem", { name: /Language/ }).click();
  const saved = answer(page, "PUT", "/api/auth/language");
  await page.getByRole("menuitemradio").nth(1).click();
  expect((await saved).ok()).toBe(true);
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-Hant");
  const me = await (await page.request.get("/api/auth/me")).json();
  expect([me.lang, me.ui_lang]).toEqual(["zh-TW", "zh-TW"]);
  await first.close();

  // Another browser, in English: the sign-in page follows the browser, and once signed in the page is theirs
  const other = await browser.newContext({ baseURL, locale: "en-US" });
  const there = await other.newPage();
  await there.goto("/login");
  expect(await htmlLang(there)).toBe("en");
  await signInAs(there, who);
  await expect(there.locator("html")).toHaveAttribute("lang", "zh-Hant");
  await expect(there.getByRole("button", { name: new RegExp(`${who.username}$`) })).toBeVisible();
  await expect(there.locator('script[src="/zh-TW.js"]')).toHaveCount(1);
  await other.close();
});
