// The Mac style (components/style/mac): its kit, and the sidebar's groups
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test } from "vitest";
import type { Drive, Located, Me, SmartFolder, Tag } from "@/api";
import { keys } from "@/api/queryKeys";
import { kits } from "@/components/style";
import { macKit } from "@/components/style/mac";
import { MacSidebar } from "@/components/style/mac/sidebar";
import { MeContext } from "@/lib/session";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

describe("the Mac style's kit", () => {
  test("is the kit of the Mac style", () => {
    expect(kits().mac).toBe(macKit);
    expect(macKit.id).toBe("mac");
  });

  test("offers Icons, List and Columns, in the toolbar rather than the status bar; Icons first", () => {
    expect(macKit.views().map((v) => v.id)).toEqual(["grid", "list", "columns"]);
    expect(
      macKit
        .views()
        .filter((v) => v.notOnPhones)
        .map((v) => v.id),
    ).toEqual(["columns"]);
    expect(macKit.defaultView).toBe("grid");
    expect(macKit.statusViews).toEqual([]);
  });

  test("follows Finder's conventions: no click on a name to rename, and new items sort into place", () => {
    expect(macKit.clickToRename).toBe(false);
    expect(macKit.newAtEnd).toBe(false);
  });
});

describe("the Mac style's sidebar", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
    localStorage.clear();
  });

  const me = { id: 1, username: "amy", role: "admin", quota_bytes: 0, used_bytes: 0, personal_root_id: "home", version: "0.6.0" } as unknown as Me;
  const located = (id: string, name: string, kind: "file" | "folder") => ({ id, name, kind, parent_id: "home" }) as Located;

  function render(who: Me = me) {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
    qc.setQueryData(keys.drives(), [
      { id: "d1", name: "My files", kind: "personal", root_id: "home" },
      { id: "d2", name: "Design team", kind: "team", root_id: "team" },
    ] as Drive[]);
    qc.setQueryData(keys.favorites("name", "asc"), [located("f1", "Reports", "folder"), located("f2", "budget.xlsx", "file")]);
    qc.setQueryData(keys.tags(), [{ id: 7, name: "Urgent", color: "red" }] as Tag[]);
    qc.setQueryData(keys.smartFolders(), [{ id: 3, name: "PDFs this year", query: {} }] as SmartFolder[]);
    const el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    act(() =>
      root!.render(
        <QueryClientProvider client={qc}>
          <MeContext.Provider value={who}>
            <MemoryRouter initialEntries={["/files/team"]}>
              <MacSidebar activeFolder="team" />
            </MemoryRouter>
          </MeContext.Provider>
        </QueryClientProvider>,
      ),
    );
  }
  /** Each group's heading, and the names of its links */
  const groups = () =>
    [...document.querySelectorAll("nav > div > section, nav > div > ul")].map((g) => [
      g.querySelector("h2")?.textContent ?? "",
      ...[...g.querySelectorAll("li a")].map((a) => a.textContent),
    ]);

  test("shows the locations in groups: Favorites, Spaces, Shared, Recent and Trash, Tags, Administration", () => {
    render();
    expect(groups()).toEqual([
      // Favourite folders, not files (those are in the list of every favourite), then smart folders
      ["Favorites", "All favorites", "Reports", "PDFs this year"],
      ["Spaces", "All spaces", "My files", "Design team"],
      ["Shared", "Shared with me", "My share links"],
      ["", "Recent", "Trash"],
      ["Tags", "Urgent"],
      ["Administration", "Control panel"],
    ]);
    // The folder open is the current one
    expect(document.querySelector('a[href="/files/team"]')!.className).toContain("bg-selection");
    expect(document.querySelector('a[href="/files/home"]')!.className).not.toContain("bg-selection");
  });

  test("new tags and new smart folders are made from their group's heading", () => {
    render();
    expect([...document.querySelectorAll("section h2 + button")].map((b) => [b.closest("section")!.querySelector("h2")!.textContent, b.getAttribute("aria-label")])).toEqual([
      ["Favorites", "New smart folder"],
      ["Tags", "New tag"],
    ]);
  });

  test("the control panel is for administrators only", () => {
    render({ ...me, role: "user" });
    expect(groups().map((g) => g[0])).not.toContain("Administration");
  });

  test("a group's heading hides what it holds, and shows it again", () => {
    render();
    const heading = () => [...document.querySelectorAll("h2 button")].find((b) => b.textContent === "Spaces") as HTMLButtonElement;
    expect(heading().getAttribute("aria-expanded")).toBe("true");
    act(() => heading().click());
    expect(heading().getAttribute("aria-expanded")).toBe("false");
    expect(groups()[1]).toEqual(["Spaces"]);
    act(() => heading().click());
    expect(groups()[1]).toHaveLength(4);
  });
});
