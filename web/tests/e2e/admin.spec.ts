// Every page of the Control panel opens from its tile, without an error, and only for administrators; and which
// release runs is told only to people who are signed in
import { expect, test } from "@playwright/test";
import { makeFolder, makeUser, signIn, uploadFile, USER_PASSWORD } from "./helpers";

const PAGES = [
  "Users",
  "Groups",
  "All share links",
  "Single sign-on",
  "Spaces",
  "Storage locations",
  "Moves",
  "Backups",
  "Replicas",
  "Storage usage",
  "General",
  "Branding",
  "Email",
  "Activity log",
  "Log settings",
];

test("folder spaces explain watching, refresh-on-open and disabled scheduled checks", async ({ page }) => {
  await signIn(page);
  const cases = [
    { watching: false, scan_minutes: 15, text: "Changes are checked when opened and every 15 minutes" },
    { watching: false, scan_minutes: 0, text: "Changes are checked when opened; scheduled checks are off" },
    { watching: true, scan_minutes: 60, text: "External changes appear within seconds" },
  ];
  for (const state of cases) {
    await page.route("**/api/admin/drives", async (route) => {
      const response = await route.fetch();
      const drives = await response.json();
      await route.fulfill({ response, json: drives.map((d: { mode: string }) => (d.mode === "folder" ? { ...d, folder_changes: state } : d)) });
    });
    await page.goto("/admin/drives");
    const status = page.getByText(state.text, { exact: true }).first();
    await expect(status).toBeVisible();
    await expect(status.locator("..")).toHaveAttribute("title", new RegExp(state.text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    await page.setViewportSize({ width: 375, height: 812 });
    await status.scrollIntoViewIfNeeded();
    await expect(status).toBeVisible();
    expect(await status.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.unroute("**/api/admin/drives");
  }
});

test("each Control panel tile opens its page, which loads without an error", async ({ page }) => {
  const failed: string[] = [];
  page.on("response", (r) => {
    if (r.url().includes("/api/") && r.status() >= 500) failed.push(`${r.status()} ${r.url()}`);
  });
  page.on("pageerror", (e) => failed.push(e.message));
  await signIn(page);
  for (const name of PAGES) {
    await page.goto("/admin");
    await page.getByRole("option", { name: new RegExp(`^${name}`) }).dblclick();
    // The address bar says where the page is: Control panel › its name
    await expect(page.getByRole("navigation", { name: "File path" }).getByText(name, { exact: true })).toBeVisible();
    await expect(page.getByText(/Couldn't load|Try again/)).toHaveCount(0);
  }
  expect(failed).toEqual([]);
});

test("old and unknown addresses under /admin go to the Control panel or the files", async ({ page }) => {
  await signIn(page);
  await page.goto("/admin/system");
  await page.waitForURL(/\/admin$/);
  await page.goto("/admin/nothing-here");
  await page.waitForURL(/\/files/);
});

test("someone who isn't an administrator is sent to their files", async ({ page }) => {
  await signIn(page);
  const name = await makeUser(page, "plain");
  await page.request.post("/api/auth/logout");
  expect((await page.request.post("/api/auth/login", { data: { username: name, password: USER_PASSWORD } })).ok()).toBe(true);
  // A password an administrator chose is replaced first
  expect((await page.request.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  await page.goto("/admin/users");
  await page.waitForURL(/\/files/);
});

test("only people who are signed in are told which release runs", async ({ page, browser, baseURL }) => {
  // Anyone may ask whether the server is up, and the answer doesn't name the release
  const health = await (await page.request.get("/api/health")).json();
  expect(health.status).toBe("ok");
  expect(health).not.toHaveProperty("version");
  const signedOut = await page.request.get("/api/auth/me");
  expect(signedOut.status()).toBe(401);
  expect(await signedOut.text()).not.toContain("version");

  await signIn(page);
  const version = (await (await page.request.get("/api/auth/me")).json()).version as string;
  expect(version).toBeTruthy();
  const line = `ThirtyFile ${version}`;

  // A quiet line at the bottom of the account menu, not something to choose
  await page.getByRole("button", { name: /^a\s*admin$/i }).click();
  const menu = page.getByRole("menu");
  await expect(menu.getByText(line, { exact: true })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: line })).toHaveCount(0);
  await page.keyboard.press("Escape");

  // The Control panel's status bar: a local build has no release notes to link to
  await page.goto("/admin");
  const status = page.locator("footer");
  await expect(status.getByText(line, { exact: true })).toBeVisible();
  if (version === "dev") await expect(status.getByRole("link")).toHaveCount(0);
  // A release links to its notes
  await page.route("**/api/auth/me", async (route) => {
    const res = await route.fetch();
    await route.fulfill({ response: res, json: { ...(await res.json()), version: "0.5.0" } });
  });
  await page.reload();
  await expect(status.getByRole("link", { name: "ThirtyFile 0.5.0" })).toHaveAttribute("href", "https://github.com/ThirtyFile/ThirtyFile/releases/tag/v0.5.0");
  await page.unroute("**/api/auth/me");

  // Pages that need no sign-in never show it: sign-in, password reset, a share link
  const folder = await makeFolder(page, "Version check");
  const file = await uploadFile(page, folder, "readme.txt", "Hello");
  const share = await page.request.post("/api/shares", { data: { node_id: file } });
  expect(share.ok()).toBe(true);
  const link = (await share.json()).id as string;
  const visitor = await browser.newContext({ baseURL });
  const guest = await visitor.newPage();
  const told: string[] = [];
  guest.on("response", async (r) => {
    const body = r.url().includes("/api/") ? await r.text().catch(() => "") : "";
    if (body.includes(`"version":"${version}"`)) told.push(r.url());
  });
  await guest.goto("/login");
  await expect(guest.getByRole("button", { name: "Click or press any key to sign in" })).toBeVisible();
  await expect(guest.getByText(line)).toHaveCount(0);
  await guest.goto("/reset-password");
  await expect(guest.getByLabel("Username or email address")).toBeVisible();
  await expect(guest.getByText(line)).toHaveCount(0);
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText("readme.txt").first()).toBeVisible();
  await expect(guest.getByText(line)).toHaveCount(0);
  expect(told).toEqual([]);
  await visitor.close();
});
