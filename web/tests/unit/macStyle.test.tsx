// The Mac style (components/style/mac): its kit, and the sidebar's groups
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, useLocation } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vitest";
import { FolderIcon } from "lucide-react";
import { api, type Drive, type Located, type Me, type SmartFolder, type Tag } from "@/api";
import { keys } from "@/api/queryKeys";
import { kits } from "@/components/style";
import { macKit } from "@/components/style/mac";
import { MacSidebar } from "@/components/style/mac/sidebar";
import { GoToFolder, PathBar, openGoToFolder } from "@/components/style/mac/pathBar";
import { MeContext } from "@/lib/session";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("sonner", () => ({ toast: { error: vi.fn<(m: string) => void>() } }));

describe("the Mac style's kit", () => {
  test("is the kit of the Mac style", () => {
    expect(kits().mac).toBe(macKit);
    expect(macKit.id).toBe("mac");
  });

  test("offers Icons, List, Columns and Gallery, in the toolbar rather than the status bar; Icons first; not Columns and Gallery on phones", () => {
    expect(macKit.views().map((v) => v.id)).toEqual(["grid", "list", "columns", "gallery"]);
    expect(
      macKit
        .views()
        .filter((v) => v.notOnPhones)
        .map((v) => v.id),
    ).toEqual(["columns", "gallery"]);
    expect(macKit.defaultView).toBe("grid");
    expect(macKit.statusViews).toEqual([]);
    // The Gallery is the Mac style's own view; the Windows style doesn't have it
    expect(Object.keys(macKit.ownViews ?? {})).toEqual(["gallery"]);
    expect(kits().windows?.ownViews).toBeUndefined();
    expect(
      kits()
        .windows?.views()
        .map((v) => v.id),
    ).not.toContain("gallery");
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

describe("the path bar and Go to folder", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });
  const at = { path: "" };
  function Where() {
    at.path = useLocation().pathname;
    return null;
  }
  function render(children: React.ReactNode) {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
    const el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    act(() =>
      root!.render(
        <QueryClientProvider client={qc}>
          <MeContext.Provider value={{ id: 1, personal_root_id: "home" } as unknown as Me}>
            <MemoryRouter initialEntries={["/files/reports"]}>
              {children}
              <Where />
            </MemoryRouter>
          </MeContext.Provider>
        </QueryClientProvider>,
      ),
    );
  }
  const crumbs = [
    { label: "All spaces", to: "/drives", virtual: true },
    { label: "My files", to: "/files/home" },
    { label: "Reports", to: "/files/reports" },
  ];

  test("shows the path, each folder above a link, the folder open last", () => {
    render(<PathBar place={{ crumbs, icon: FolderIcon, path: "/My files/Reports", searchPlaceholder: "" }} />);
    const bar = document.querySelector('nav[aria-label="Path bar"]')!;
    expect([...bar.querySelectorAll("a")].map((a) => [a.textContent, a.getAttribute("href")])).toEqual([
      ["All spaces", "/drives"],
      ["My files", "/files/home"],
    ]);
    expect(bar.querySelector('[aria-current="page"]')!.textContent).toBe("Reports");
  });

  test("Go to folder starts with the path of the folder open, and goes to the one typed", async () => {
    const find = vi.spyOn(api, "findPath").mockResolvedValueOnce({ place: "folder", id: "plans" } as never);
    render(<GoToFolder path="/My files/Reports" />);
    act(() => openGoToFolder());
    const input = document.querySelector<HTMLInputElement>('[role="dialog"] input')!;
    expect(input.value).toBe("/My files/Reports");
    const type = (text: string) =>
      act(() => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, text);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
    type("/My files/Plans");
    await act(async () => input.form!.requestSubmit());
    expect(find).toHaveBeenCalledWith("/My files/Plans", expect.anything());
    expect(at.path).toBe("/files/plans");
    expect(document.querySelector('[role="dialog"]')).toBeNull();
  });
});
