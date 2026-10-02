// A large folder a part at a time (useFolderWindows in lib/windows.ts), against a server stood in for: which parts load,
// which are kept, and what happens when the folder changes meanwhile or a part fails
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi, type MockInstance } from "vitest";
import { api, type Node } from "@/api";
import { KEEP, WINDOW, useFolderWindows, windowsKey } from "@/lib/windows";

// Parts load in the background, as the query client resolves them: the tests wait for what they expect instead of act()
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;

const node = (i: number): Node => ({
  id: `n${i}`,
  parent_id: "big",
  kind: i % 10 === 0 ? "folder" : "file",
  name: `Item ${i}`,
  size: 1,
  mime: "text/plain",
  created_at: 1_700_000_000,
  updated_at: 1_700_000_000,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "amy",
  is_favorite: false,
});

/** The folder on the server: how many items it has now, and parts that fail */
const folder = { total: 1700, failing: new Set<number>() };
let childrenAt: MockInstance<typeof api.childrenAt>;
let position: MockInstance<typeof api.position>;

type Windows = ReturnType<typeof useFolderWindows>;
const out: { current: Windows | null } = { current: null };
function Harness({ id, enabled }: { id: string; enabled: boolean }) {
  out.current = useFolderWindows(id, "name", "asc", enabled);
  return null;
}
const w = () => out.current!;

let qc: QueryClient;
let root: Root;
async function mount(id = "big", enabled = true) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={qc}>
        <Harness id={id} enabled={enabled} />
      </QueryClientProvider>,
    ),
  );
}
/** The starts of the parts asked for, in order */
const asked = () => childrenAt.mock.calls.map(([, , , offset]) => offset);
/** The starts of the parts cached for the folder */
const cached = () =>
  qc
    .getQueryCache()
    .findAll({ queryKey: windowsKey("big", "name", "asc") })
    .map((q) => q.queryKey[5] as number)
    .sort((a, b) => a - b);

beforeEach(() => {
  folder.total = 1700;
  folder.failing.clear();
  childrenAt = vi.spyOn(api, "childrenAt").mockImplementation(async (_id, _sort, _order, offset, limit) => {
    if (folder.failing.has(offset)) throw new Error("The server didn't answer");
    const items = Array.from({ length: Math.max(0, Math.min(limit, folder.total - offset)) }, (_, k) => node(offset + k));
    return { items, next: null, total: folder.total };
  });
  position = vi.spyOn(api, "position").mockResolvedValue({ position: 1234, total: 1700 });
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  root = createRoot(document.createElement("div"));
});
afterEach(() => {
  act(() => root.unmount());
  qc.clear();
  vi.restoreAllMocks();
});

describe("a large folder a part at a time", () => {
  test("loads the first part, then the next one once it knows how many items there are", async () => {
    await mount();
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    expect(asked()).toEqual([0, WINDOW]);
    const { list } = w();
    expect(list.total).toBe(1700);
    expect(list.at).toHaveLength(1700);
    expect(list.at[WINDOW + 1]?.id).toBe(`n${WINDOW + 1}`);
    expect(list.at[1600]).toBeUndefined();
    expect(list.complete).toBe(false);
    expect(w().isLoading).toBe(false);
  });

  test("loads the parts in view and one more each way, not those before them", async () => {
    await mount();
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    // End: the last items are shown
    act(() => w().list.show(1650, 1699));
    await vi.waitFor(() => expect(w().list.complete).toBe(true));
    expect(asked()).toEqual([0, 500, 1000, 1500]);
    expect(w().list.loaded.map((n) => n.id)).toEqual(Array.from({ length: 1700 }, (_, i) => `n${i}`));
    expect(w().loadingMore).toBe(false);
  });

  test("finds where an item is: at once when it is loaded, otherwise from the server", async () => {
    await mount();
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    expect(await w().list.locate("n42")).toBe(42);
    expect(position).not.toHaveBeenCalled();
    expect(await w().list.locate("n1234")).toBe(1234);
    expect(position).toHaveBeenCalledWith("big", "n1234", "name", "asc");
  });

  test("parts loaded before the folder changed size load again, and the items show where the newest part has them", async () => {
    await mount();
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    // An item was added meanwhile: the next part says so
    folder.total = 1701;
    act(() => w().list.show(1000, 1000));
    await vi.waitFor(() => expect(w().list.total).toBe(1701));
    // The part in view before loads again; the first, out of view now, is forgotten (and loads when shown)
    await vi.waitFor(() => expect(asked().filter((s) => s === WINDOW).length).toBeGreaterThanOrEqual(2));
    expect(cached()).not.toContain(0);
    await vi.waitFor(() => expect(w().list.loaded.length).toBeGreaterThan(WINDOW));
    expect(w().list.at[1700]?.id).toBe("n1700");
  });

  test("keeps at most KEEP parts, the first and those nearest the ones shown", async () => {
    folder.total = 30 * WINDOW;
    await mount();
    await vi.waitFor(() => expect(w().list.total).toBe(30 * WINDOW));
    for (let part = 1; part < 30; part++) {
      act(() => w().list.show(part * WINDOW, part * WINDOW));
      await vi.waitFor(() => expect(w().list.at[part * WINDOW]).toBeDefined());
    }
    expect(cached().length).toBeLessThanOrEqual(KEEP);
    expect(cached()).toContain(0);
    expect(cached()).toContain(29 * WINDOW);
    expect(cached()).not.toContain(WINDOW);
    expect(w().list.loaded.length).toBeLessThan(30 * WINDOW);
  });

  test("a folder that can't be listed says so, and loads when tried again", async () => {
    folder.failing.add(0);
    await mount();
    await vi.waitFor(() => expect(w().error?.message).toBe("The server didn't answer"));
    expect(w().isLoading).toBe(false);
    expect(w().partError).toBeNull();
    folder.failing.clear();
    await act(() => w().retry());
    await vi.waitFor(() => expect(w().list.total).toBe(1700));
    expect(w().error).toBeNull();
  });

  test("a part that can't be loaded leaves the rest shown, and loads in its place when tried again", async () => {
    folder.failing.add(WINDOW);
    await mount();
    await vi.waitFor(() => expect(w().partError?.message).toBe("The server didn't answer"));
    expect(w().error).toBeNull();
    expect(w().list.loaded).toHaveLength(WINDOW);
    folder.failing.clear();
    await act(() => w().retry());
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    expect(w().partError).toBeNull();
    // Each item shows once
    expect(new Set(w().list.loaded.map((n) => n.id)).size).toBe(2 * WINDOW);
  });

  test("another folder starts at its top; a list turned off asks for nothing", async () => {
    await mount();
    await vi.waitFor(() => expect(w().list.loaded).toHaveLength(2 * WINDOW));
    act(() => w().list.show(1650, 1699));
    await vi.waitFor(() => expect(w().list.complete).toBe(true));
    childrenAt.mockClear();
    await mount("other");
    await vi.waitFor(() => expect(w().list.total).toBe(1700));
    expect(childrenAt.mock.calls.map(([id, , , offset]) => [id, offset])).toEqual([
      ["other", 0],
      ["other", WINDOW],
    ]);

    childrenAt.mockClear();
    await mount("third", false);
    expect(childrenAt).not.toHaveBeenCalled();
    expect(w().list.total).toBe(-1);
    expect(w().isLoading).toBe(false);
  });
});
