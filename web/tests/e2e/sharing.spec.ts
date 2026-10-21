// Sharing a folder by link, seen by a visitor who isn't signed in: the password, browsing, downloading, and the link
// stopping once it is deleted; and sharing a folder with a contributor, who changes only what they add
import { readFile } from "node:fs/promises";
import { devices, expect, test } from "@playwright/test";
import { USER_PASSWORD, answer, makeFolder, makeUser, openFolder, signIn, signInAsNewUser, unique, uploadFile, uploadFinished } from "./helpers";

test("a visitor unlocks a shared folder with its password, opens a folder in it and downloads a file", async ({ page, browser, baseURL }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Shared by link");
  const sub = await makeFolder(page, "Minutes", dir);
  await uploadFile(page, dir, "agenda.txt", "Agenda for Monday");
  await uploadFile(page, sub, "2026-09.txt", "Minutes of September");
  const res = await page.request.post("/api/shares", { data: { node_id: dir, password: "a-share-password-1" } });
  expect(res.ok()).toBe(true);
  const link = (await res.json()).id as string;

  // Another browser, signed in nowhere
  const visitor = await browser.newContext({ baseURL, acceptDownloads: true });
  const guest = await visitor.newPage();
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText("This share requires a password")).toBeVisible();
  await guest.getByLabel("Password").fill("not the password");
  await guest.getByRole("button", { name: "Open", exact: true }).click();
  await expect(guest.getByRole("alert")).toBeVisible();
  await guest.getByLabel("Password").fill("a-share-password-1");
  await guest.getByRole("button", { name: "Open", exact: true }).click();

  const row = (name: string) => guest.locator("[data-node-id]").filter({ hasText: name });
  await expect(row("agenda.txt")).toBeVisible();
  await expect(guest.getByText("Shared by admin")).toBeVisible();
  await row("Minutes").dblclick();
  await expect(row("2026-09.txt")).toBeVisible();

  await row("2026-09.txt").click();
  const downloading = guest.waitForEvent("download");
  await guest.getByRole("button", { name: "Download 1 item" }).first().click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("2026-09.txt");
  expect(await readFile(await download.path(), "utf8")).toBe("Minutes of September");

  // Deleted by its owner, the link no longer opens
  expect((await page.request.delete(`/api/shares/${link}`)).ok()).toBe(true);
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText(/doesn't exist or has expired/)).toBeVisible();
  await visitor.close();
});

test("a link to one file shows it to a visitor on a phone, who can download it", async ({ page, browser, baseURL }) => {
  await signIn(page);
  const dir = await makeFolder(page, "One file");
  const id = await uploadFile(page, dir, "notes.txt", "Just this file");
  const res = await page.request.post("/api/shares", { data: { node_id: id } });
  expect(res.ok()).toBe(true);
  const link = (await res.json()).id as string;

  const visitor = await browser.newContext({ ...devices["Pixel 7"], baseURL, acceptDownloads: true });
  const guest = await visitor.newPage();
  await guest.goto(`/share/${link}`);
  await expect(guest.getByText("notes.txt").first()).toBeVisible();
  // Nothing wider than the phone's screen
  expect(await guest.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const downloading = guest.waitForEvent("download");
  await guest.getByRole("button", { name: "Download", exact: true }).first().click();
  const download = await downloading;
  expect(await readFile(await download.path(), "utf8")).toBe("Just this file");
  await visitor.close();
});

for (const phone of [false, true]) {
  test(`a share link's password stays hidden until shown, and editing keeps, changes or removes it${phone ? " on a phone" : ""}`, async ({ page, browser, baseURL }) => {
    if (phone) await page.setViewportSize({ width: 390, height: 844 });
    await signIn(page);
    const dir = await makeFolder(page, `Hidden password ${phone ? "phone" : "desktop"}`);
    await uploadFile(page, dir, "plan.txt", "The plan");
    await page.goto(`/files/${dir}`);
    const share = async () => {
      await page.locator("[data-node-id]").filter({ hasText: "plan.txt" }).click({ button: "right" });
      await page.getByRole("menuitem", { name: "Create share link" }).click();
      return page.getByRole("dialog", { name: "Share link for “plan.txt”" });
    };
    let dialog = await share();
    let field = dialog.getByLabel("Password (optional)");
    // Dots, and not filled in with the account's own password
    await expect(field).toHaveAttribute("type", "password");
    await expect(field).toHaveAttribute("autocomplete", "new-password");
    await field.fill("first-share-pass-1");
    const show = dialog.getByRole("button", { name: "Show password", exact: true });
    await expect(show).toHaveAttribute("aria-pressed", "false");
    await show.click();
    await expect(show).toHaveAttribute("aria-pressed", "true");
    await expect(field).toHaveAttribute("type", "text");
    await expect(field).toHaveValue("first-share-pass-1");
    await show.click();
    await expect(field).toHaveAttribute("type", "password");
    // Shown, then the dialog closed and opened again: hidden again
    await show.click();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    dialog = await share();
    field = dialog.getByLabel("Password (optional)");
    await expect(field).toHaveAttribute("type", "password");
    await field.fill("first-share-pass-1");
    const created = answer(page, "POST", "/api/shares");
    await dialog.getByRole("button", { name: "Create link", exact: true }).click();
    const link = (await (await created).json()).id as string;
    const visitor = await browser.newContext({ baseURL });
    const opens = async (password: string) => (await visitor.request.post(`/api/public/shares/${link}/unlock`, { data: { password } })).ok();
    expect(await opens("first-share-pass-1")).toBe(true);

    const edit = async (change: (dialog: ReturnType<typeof page.getByRole>) => Promise<void>) => {
      await dialog.getByRole("button", { name: "Edit link", exact: true }).click();
      const editing = page.getByRole("dialog", { name: "Edit link for “plan.txt”" });
      const replacement = editing.getByLabel("New password (leave blank to keep the current one)");
      await expect(replacement).toHaveAttribute("type", "password");
      await expect(replacement).toHaveValue("");
      await change(editing);
      const saved = answer(page, "PATCH", `/api/shares/${link}`);
      await editing.getByRole("button", { name: "Save", exact: true }).click();
      expect((await saved).ok()).toBe(true);
      await expect(editing).toBeHidden();
    };
    // Left blank: the password stays
    await edit(async () => {});
    expect(await opens("first-share-pass-1")).toBe(true);
    // A new one, checked with Show before saving
    await edit(async (editing) => {
      const replacement = editing.getByLabel("New password (leave blank to keep the current one)");
      await replacement.fill("second-share-pass-2");
      await editing.getByRole("button", { name: "Show password", exact: true }).click();
      await expect(replacement).toHaveAttribute("type", "text");
      await expect(replacement).toHaveValue("second-share-pass-2");
    });
    expect(await opens("first-share-pass-1")).toBe(false);
    expect(await opens("second-share-pass-2")).toBe(true);
    // Removed: the field and its Show button can't be used
    await edit(async (editing) => {
      await editing.getByText("Remove the password", { exact: true }).click();
      await expect(editing.getByLabel("New password (leave blank to keep the current one)")).toBeDisabled();
      await expect(editing.getByRole("button", { name: "Show password", exact: true })).toBeDisabled();
    });
    // Asked of a visitor who hasn't unlocked it before
    const fresh = await browser.newContext({ baseURL });
    expect((await (await fresh.request.get(`/api/public/shares/${link}`)).json()).needs_password).toBe(false);
    await fresh.close();
    if (phone) expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await visitor.close();
  });
}

test("Share with… picks a person with the keyboard alone", async ({ page }) => {
  await signIn(page);
  const kim = await makeUser(page, "kim");
  // In a folder of its own: My files holds the other tests' folders too
  const parent = await makeFolder(page, "Sharing by keyboard");
  const dir = await makeFolder(page, "Shared with Kim", parent);
  await page.goto(`/files/${parent}`);
  await page.locator("[data-node-id]").filter({ hasText: "Shared with Kim" }).first().click();
  await page.getByRole("button", { name: "Share with…" }).click();
  const dialog = page.getByRole("dialog");
  const box = dialog.getByRole("combobox", { name: "Username or group name" });
  await box.focus();
  await page.keyboard.type(kim);
  await expect(box).toHaveAttribute("aria-expanded", "true");
  const option = dialog.getByRole("option", { name: new RegExp(kim) });
  await expect(option).toBeVisible();
  // The arrows go through what was found; the box keeps the focus and says which one they are on
  await page.keyboard.press("ArrowDown");
  await expect(box).toBeFocused();
  await expect(option).toHaveAttribute("aria-selected", "true");
  await expect(box).toHaveAttribute("aria-activedescendant", (await option.getAttribute("id"))!);
  // Escape closes the list, not the dialog
  await page.keyboard.press("Escape");
  await expect(box).toHaveAttribute("aria-expanded", "false");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(option).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  // Picked: the focus moves on to the role, then Add
  await expect(dialog.getByRole("combobox", { name: "Role" }).first()).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  await expect(dialog.getByRole("button", { name: "Add" })).toBeFocused();
  // Added once the server has answered: it may wait its turn behind other tests' changes
  const added = answer(page, "POST", `/api/nodes/${dir}/access`);
  await page.keyboard.press("Enter");
  expect((await added).ok()).toBe(true);
  expect(JSON.stringify(await (await page.request.get(`/api/nodes/${dir}/access`)).json())).toContain(kim);
});

test("a contributor adds a file and renames it, but can't rename or delete someone else's", async ({ page }) => {
  await signIn(page);
  const dir = await makeFolder(page, "Hand in");
  await uploadFile(page, dir, "brief.txt", "The brief");
  const username = await makeUser(page, "contrib");
  const [user] = (await (await page.request.get(`/api/admin/users?q=${username}`)).json()) as { id: number }[];
  const granted = await page.request.post(`/api/nodes/${dir}/access`, { data: { principal_type: "user", principal_id: user.id, role: "contributor" } });
  expect(granted.ok()).toBe(true);

  // Signed in as the contributor, in the Windows style (its toolbar names the commands)
  expect((await page.request.post("/api/auth/logout")).ok()).toBe(true);
  expect((await page.request.post("/api/auth/login", { data: { username, password: USER_PASSWORD } })).ok()).toBe(true);
  expect((await page.request.put("/api/auth/password", { data: { current: USER_PASSWORD, new: `${USER_PASSWORD}-2` } })).ok()).toBe(true);
  expect((await page.request.put("/api/auth/style", { data: { style: "windows" } })).ok()).toBe(true);
  await page.evaluate(() => localStorage.setItem("tf-view", JSON.stringify("list")));
  await openFolder(page, dir);
  const uploaded = uploadFinished(page);
  await page.locator('input[type="file"][multiple]').setInputFiles([{ name: "my part.txt", mimeType: "text/plain", buffer: Buffer.from("My part") }]);
  await uploaded;
  await expect(page.getByRole("status").filter({ hasText: "1 upload complete" })).toBeAttached();
  const row = (name: string) => page.locator("[data-node-id]").filter({ has: page.getByText(name, { exact: true }) });
  const rename = page.getByRole("button", { name: "Rename", exact: true });
  const remove = page.getByRole("button", { name: "Delete", exact: true });

  // Someone else's file: neither renamed nor deleted
  await row("brief.txt").click();
  await expect(rename).toBeDisabled();
  await expect(remove).toBeDisabled();

  // Their own: renamed
  await row("my part.txt").click();
  await expect(remove).toBeEnabled();
  await rename.click();
  const box = page.getByRole("textbox", { name: "New name" });
  await box.fill("final part.txt");
  const renamed = answer(page, "PATCH", /^\/api\/nodes\/[^/]+$/);
  await box.press("Enter");
  expect((await renamed).ok()).toBe(true);
  await expect(row("final part.txt")).toBeVisible();

  // Both selected: Delete waits for a selection that is all theirs
  await row("brief.txt").click({ modifiers: ["ControlOrMeta"] });
  await expect(remove).toBeDisabled();
});

test("the list of share links sorts by its headers and says where each linked folder is", async ({ page }) => {
  await signInAsNewUser(page, "links");
  const root = (await (await page.request.get("/api/auth/me")).json()).root_id as string;
  const [teamAName, teamBName] = [`Team A ${unique()}`, `Team B ${unique()}`];
  const teamA = await makeFolder(page, teamAName, root);
  const teamB = await makeFolder(page, teamBName, root);
  const sameA = await makeFolder(page, "XXX", teamA);
  const sameB = await makeFolder(page, "XXX", teamB);
  const top = await makeFolder(page, "Alpha", root);
  const link = async (node: string) => (await (await page.request.post("/api/shares", { data: { node_id: node } })).json()).id as string;
  for (const node of [sameA, sameB, top]) await link(node);

  await page.goto("/shares");
  const rows = page.locator("tr[data-row-id]");
  await expect(rows).toHaveCount(3);
  // The two folders called XXX are told apart by the folder they are in
  const xxx = rows.filter({ hasText: "XXX" });
  await expect(xxx).toHaveCount(2);
  await expect(xxx.filter({ hasText: `My files/${teamAName}` })).toHaveCount(1);
  await expect(xxx.filter({ hasText: `My files/${teamBName}` })).toHaveCount(1);
  await expect(rows.filter({ hasText: "Alpha" }).locator("td").nth(1)).toHaveText("My files");

  // Sorting by the name header, then the other way, and the choice is still there after a reload
  const names = () =>
    rows
      .locator("td:first-child")
      .allInnerTexts()
      .then((all) => all.map((n) => n.trim().replace(/ \w+$/, "")));
  await page.getByRole("columnheader", { name: "Name" }).getByRole("button").click();
  await expect(page.getByRole("columnheader", { name: "Name" })).toHaveAttribute("aria-sort", "ascending");
  expect((await names()).map((n) => n.split(" ")[0])).toEqual(["Alpha", "XXX", "XXX"]);
  await page.getByRole("columnheader", { name: "Name" }).getByRole("button").click();
  await expect(page.getByRole("columnheader", { name: "Name" })).toHaveAttribute("aria-sort", "descending");
  expect((await names()).map((n) => n.split(" ")[0])).toEqual(["XXX", "XXX", "Alpha"]);
  await page.reload();
  await expect(page.getByRole("columnheader", { name: "Name" })).toHaveAttribute("aria-sort", "descending");

  // By location, the folders called XXX sit next to each other in the order of their folders
  await page.getByRole("columnheader", { name: "Location" }).getByRole("button").click();
  await expect(page.getByRole("columnheader", { name: "Location" })).toHaveAttribute("aria-sort", "ascending");
  const places = await rows.locator("td:nth-child(2)").allInnerTexts();
  expect(places).toEqual([...places].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" })));

  // A click on the place opens that folder
  await xxx
    .filter({ hasText: `My files/${teamBName}` })
    .getByRole("link")
    .click();
  await expect(page).toHaveURL(new RegExp(`/files/${teamB}$`));
});
