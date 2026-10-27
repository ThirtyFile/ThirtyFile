// Storage analytics: the overview, the largest files, copies that can't all be chosen, old folders, and the treemap
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vite-plus/test";
import type { AnalyticsFile, AnalyticsSnapshot, AnalyticsTreeNode } from "@/api";
import { AnalyticsPage } from "@/pages/AnalyticsPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/components/Frame", () => ({
  Frame: ({ children, toolbar }: { children: ReactNode; toolbar: ReactNode }) => (
    <main>
      {toolbar}
      {children}
    </main>
  ),
  ToolButton: ({ label, onClick, disabled }: { label: string; onClick(): void; disabled?: boolean }) => (
    <button type="button" disabled={disabled} onClick={onClick}>
      {label}
    </button>
  ),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const NOW = 1_800_000_000;
const YEAR = 365 * 86400;
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

const file = (id: string, name: string, size: number, folder = "/Projects"): AnalyticsFile => ({ id, name, folder_id: `folder-${id}`, folder, owner: "amy", size, updated_at: NOW - 86400 });
const node = (name: string, bytes: number, children: AnalyticsTreeNode[] = [], kind: AnalyticsTreeNode["kind"] = "folder"): AnalyticsTreeNode => ({
  id: kind === "folder" ? `id-${name}` : null,
  name,
  bytes,
  files: bytes / 100,
  children,
  kind,
});

function snapshot(over: Partial<AnalyticsSnapshot> = {}): AnalyticsSnapshot {
  return {
    drive_id: "d1",
    name: "Team",
    taken_at: NOW,
    folder_space: false,
    files: 12,
    folders: 4,
    live_bytes: 9000,
    kinds: [
      { kind: "pictures", files: 4, bytes: 6000 },
      { kind: "video", files: 0, bytes: 0 },
      { kind: "audio", files: 0, bytes: 0 },
      { kind: "documents", files: 8, bytes: 3000 },
      { kind: "archives", files: 0, bytes: 0 },
      { kind: "other", files: 0, bytes: 0 },
    ],
    trash_bytes: 2048,
    trash_files: 2,
    version_bytes: 5000,
    old_version_bytes: 1024,
    stale_bytes: 3000,
    stale_files: 3,
    largest: [file("a", "video-big.mov", 5000), file("b", "report.pdf", 1200, "/Docs")],
    duplicates: {
      wasted_bytes: 4000,
      partial: false,
      groups: [{ size: 2000, copies: 3, wasted: 4000, files: [file("c1", "copy-1.bin", 2000), file("c2", "copy-2.bin", 2000), file("c3", "copy-3.bin", 2000)] }],
    },
    folders_by_size: [
      { id: "old1", path: "/Archive/2019", bytes: 4000, files: 9, newest: NOW - 6 * YEAR },
      { id: "old2", path: "/Archive/2022", bytes: 3000, files: 5, newest: NOW - Math.round(1.5 * YEAR) },
      { id: "new1", path: "/Projects", bytes: 2000, files: 4, newest: NOW - 86400 },
    ],
    tree: node("Team", 9000, [node("Projects", 5000, [node("2024", 3000), node("Files here", 2000, [], "files")]), node("Docs", 3000), node("Other", 1000, [], "other")]),
    ...over,
  };
}

let root: Root | null = null;
let host: HTMLElement | undefined;
let calls: { url: string; method: string; body?: string }[];

async function mount(snap: AnalyticsSnapshot | null) {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      calls.push({ url, method: init?.method ?? "GET", body: init?.body ? String(init.body) : undefined });
      if (url.endsWith("/api/drives/d1/analytics") && (init?.method ?? "GET") === "GET")
        return json({ drive: { id: "d1", name: "Team", kind: "team", folder_space: false }, snapshot: snap });
      if (url.endsWith("/api/drives/d1/analytics")) return json({ id: "job1", kind: "analytics", state: "done", done: 1, total: 1, error: null, node_id: null, name: null });
      if (url.endsWith("/api/nodes/trash")) return json({});
      return json({}, 404);
    }),
  );
  const el = document.createElement("div");
  document.body.append(el);
  host = el;
  root = createRoot(el);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/analytics/d1"]}>
          <Routes>
            <Route path="/analytics/:drive" element={<AnalyticsPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(text()).toMatch(/As of|Nothing has been worked out/));
  });
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  vi.unstubAllGlobals();
});

const text = () => host?.textContent ?? "";
const button = (name: string) => {
  const found = [...host!.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
  expect(found, `button ${name}`).toBeDefined();
  return found!;
};
const tab = (name: string) => [...host!.querySelectorAll<HTMLElement>('[role="tab"]')].find((b) => b.textContent?.startsWith(name))!;
const open = (name: string) => act(async () => tab(name).click());

describe("Storage analytics", () => {
  test("before the first time it says so and offers to work it out", async () => {
    await mount(null);
    expect(text()).toContain("Nothing has been worked out for Team yet.");
    expect(button("Work it out")).toBeDefined();
    expect(host!.querySelector('[role="tablist"]')).toBeNull();
  });

  test("works it out as a task, then reads the snapshot again", async () => {
    await mount(null);
    await act(async () => button("Work it out").click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.filter((c) => c.url.endsWith("/analytics") && c.method === "GET").length).toBeGreaterThanOrEqual(2));
    });
    expect(calls.some((c) => c.method === "POST" && c.url.endsWith("/api/drives/d1/analytics"))).toBe(true);
  });

  test("the overview: totals, kinds, and what could be cleaned up with sizes", async () => {
    await mount(snapshot());
    expect(text()).toContain("As of");
    for (const part of [
      "8.8 KB",
      "2.0 KB",
      "Pictures",
      "Documents",
      "Empty the trash (2.0 KB)",
      "Earlier versions replaced more than a year ago (1.0 KB)",
      "Files not changed for a year (2.9 KB)",
      "Copies of the same file (3.9 KB)",
    ]) {
      expect(text().includes(part)).toBe(true);
    }
    // Nothing to clean up: it says so
    act(() => root?.unmount());
    host?.remove();
    await mount(snapshot({ trash_bytes: 0, old_version_bytes: 0, stale_bytes: 0, duplicates: { groups: [], wasted_bytes: 0, partial: false } }));
    expect(text()).toContain("Nothing stands out.");
  });

  test("the largest files, each with where it is and a way to the trash", async () => {
    await mount(snapshot());
    await open("Largest files");
    expect(text()).toContain("video-big.mov");
    expect(text()).toContain("/Docs");
    expect(host!.querySelectorAll('a[href="/files/folder-a"]')).toHaveLength(1);
    const first = host!.querySelector('ul[aria-label="Largest files"] li')!;
    await act(async () => [...first.querySelectorAll("button")].find((b) => b.textContent === "Move to the trash")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/nodes/trash"))).toBe(true));
    });
    expect(JSON.parse(calls.find((c) => c.url.endsWith("/api/nodes/trash"))!.body!)).toEqual({ ids: ["a"] });
    await act(async () => {
      await vi.waitFor(() => expect(text()).not.toContain("video-big.mov"));
    });
  });

  test("copies: one always stays, nothing is chosen for you, and the choice goes to the trash", async () => {
    await mount(snapshot());
    await open("Duplicates");
    const boxes = () => [...host!.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
    expect(boxes()).toHaveLength(3);
    expect(boxes().every((b) => !b.checked)).toBe(true);
    expect(button("Move selected to the trash").disabled).toBe(true);
    expect(text()).toContain("copies take quota but not disk space");
    await act(async () => boxes()[0].click());
    await act(async () => boxes()[1].click());
    // Two of three chosen: the last copy can't be, or nothing would be left
    expect(boxes()[2].disabled).toBe(true);
    expect(button("Move selected to the trash").disabled).toBe(false);
    await act(async () => button("Move selected to the trash").click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/nodes/trash"))).toBe(true));
    });
    expect(JSON.parse(calls.find((c) => c.url.endsWith("/api/nodes/trash"))!.body!).ids.sort()).toEqual(["c1", "c2"]);
  });

  test("in a folder space the copies are files on the disk, and a partial reading says so", async () => {
    await mount(snapshot({ folder_space: true, duplicates: { ...snapshot().duplicates, partial: true } }));
    await open("Duplicates");
    expect(text()).toContain("Each copy is a file on the disk");
    expect(text()).toContain("Only some of the files with the same size were read");
  });

  test("old folders: those where nothing changed for the years chosen", async () => {
    await mount(snapshot());
    await open("Old folders");
    expect(text()).toContain("/Archive/2019");
    expect(text()).toContain("/Archive/2022");
    expect(text()).not.toContain("/Projects");
    const years = host!.querySelector<HTMLSelectElement>("#old-years")!;
    await act(async () => {
      years.value = "2";
      years.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(text()).toContain("/Archive/2019");
    expect(text()).not.toContain("/Archive/2022");
    await act(async () => {
      years.value = "5";
      years.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(text()).toContain("/Archive/2019");
  });

  test("the treemap: blocks and a list, going into a folder and back", async () => {
    await mount(snapshot());
    await open("Treemap");
    expect(host!.querySelector('svg[role="img"]')).not.toBeNull();
    const list = () => [...host!.querySelectorAll('ul[aria-label="Folders and their sizes"] li')].map((li) => li.textContent);
    expect(list().map((l) => l?.split("·")[0])).toEqual(["Projects4.9 KB ", "Docs2.9 KB ", "Other folders1,000 B "]);
    await act(async () => host!.querySelector<HTMLButtonElement>('ul[aria-label="Folders and their sizes"] li button')!.click());
    expect(host!.querySelector('nav[aria-label="Folders gone into"]')!.textContent).toContain("Projects");
    expect(list()).toHaveLength(2);
    expect(list()[0]!.startsWith("2024")).toBe(true);
    expect(list()[1]!.startsWith("Files here")).toBe(true);
    await act(async () => [...host!.querySelectorAll<HTMLButtonElement>('nav[aria-label="Folders gone into"] button')].find((b) => b.textContent === "Team")!.click());
    expect(list()).toHaveLength(3);
    // By the number of files
    await act(async () => button("Number of files").click());
    expect(list()[0]).toContain("50");
  });
});
