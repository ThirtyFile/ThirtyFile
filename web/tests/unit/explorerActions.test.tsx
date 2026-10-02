// The explorer's commands (components/explorer/actions.ts) over what is selected (explorer/state.ts): in a large folder
// not all loaded, a selection can be a span whose ids the server gives a batch at a time. The server is stood in for.
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api, type Job, type Me, type Node } from "@/api";
import type { ExplorerProps } from "@/components/Explorer";
import { setClipboard } from "@/lib/clipboard";
import type { SparseList } from "@/lib/windows";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const toast = vi.hoisted(() =>
  Object.assign(vi.fn<(...a: unknown[]) => void>(), {
    success: vi.fn<(...a: unknown[]) => void>(),
    error: vi.fn<(...a: unknown[]) => void>(),
    loading: vi.fn<(...a: unknown[]) => void>(),
    dismiss: vi.fn<(...a: unknown[]) => void>(),
  }),
);
vi.mock("sonner", () => ({ toast }));
const confirm = vi.hoisted(() => vi.fn<(o: unknown) => Promise<boolean>>());
vi.mock("@/lib/confirm", () => ({ confirm }));
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));
const me = vi.hoisted(() => ({ id: 1, username: "amy", role: "user", can_write: true, can_delete: true, can_share: true }));
vi.mock("@/lib/session", async () => {
  const { useState: state } = await import("react");
  return { useMe: () => me as unknown as Me, usePersisted: <T,>(_key: string, initial: T) => state(initial) };
});
vi.mock("@/tabs", () => ({ useTabActions: () => ({ openFile: vi.fn<(path: string) => void>() }) }));
vi.mock("react-router", () => ({ useNavigate: () => vi.fn<(path: string) => void>() }));

const { useExplorerState } = await import("@/components/explorer/state");
const { useExplorerActions } = await import("@/components/explorer/actions");

/** A folder of 2,500 items of which only the first three are loaded */
const TOTAL = 2500;
const node = (id: string): Node => ({
  id,
  parent_id: "big",
  kind: "file",
  name: `${id}.txt`,
  size: 1,
  mime: "text/plain",
  created_at: 1_700_000_000,
  updated_at: 1_700_000_000,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "amy",
  is_favorite: false,
});
const ITEMS = ["a", "b", "c"].map(node);
const ALL = Array.from({ length: TOTAL }, (_, i) => (i < 3 ? ITEMS[i].id : `n${i}`));
const list: SparseList<Node> = {
  at: ITEMS,
  total: TOTAL,
  loaded: ITEMS,
  index: new Map(ITEMS.map((n, i) => [n.id, i])),
  complete: false,
  show: () => {},
  locate: () => Promise.resolve(null),
};
const props = (folderId: string): ExplorerProps => ({
  items: folderId === "big" ? ITEMS : [],
  list: folderId === "big" ? list : undefined,
  loading: false,
  folderId,
  role: "editor",
  crumbs: [{ label: folderId }],
  sort: { key: "name", order: "asc" },
});

type Hooks = { s: ReturnType<typeof useExplorerState>; a: ReturnType<typeof useExplorerActions>; go(folder: string): void };
const out: { current: Hooks | null } = { current: null };
function Harness() {
  const [folder, go] = useState("big");
  const p = props(folder);
  const s = useExplorerState(p);
  const a = useExplorerActions(p, s);
  out.current = { s, a, go };
  return null;
}
const hooks = () => out.current!;

let root: Root;
let container: HTMLElement;
beforeEach(async () => {
  vi.clearAllMocks();
  confirm.mockResolvedValue(true);
  container = document.createElement("div");
  root = createRoot(container);
  await act(async () =>
    root.render(
      <QueryClientProvider client={new QueryClient()}>
        <Harness />
      </QueryClientProvider>,
    ),
  );
});
afterEach(() => {
  act(() => root.unmount());
  setClipboard(null);
  vi.restoreAllMocks();
});

const done = (kind: Job["kind"]): Job => ({ id: "", kind, state: "done", done: 1, total: 1, error: null, node_id: null, name: null }) as Job;
/** The server: the span's ids a page of 1,000 at a time, and every change working */
function server() {
  const selection = vi.spyOn(api, "selection").mockImplementation(async (_folder, req) => {
    const start = req.after ? Number(req.after) : 0;
    const ids = ALL.filter((id) => !req.except.includes(id)).slice(start, start + 1000);
    return { ids, next: start + 1000 < TOTAL ? String(start + 1000) : null };
  });
  return {
    selection,
    trash: vi.spyOn(api, "trash").mockResolvedValue(undefined),
    restore: vi.spyOn(api, "restore").mockResolvedValue(undefined),
    deleteForever: vi.spyOn(api, "deleteForever").mockResolvedValue(done("delete")),
    conflicts: vi.spyOn(api, "conflicts").mockResolvedValue([]),
    move: vi.spyOn(api, "move").mockResolvedValue(done("move")),
    copy: vi.spyOn(api, "copy").mockResolvedValue(done("copy")),
    setFavorite: vi.spyOn(api, "setFavorite").mockResolvedValue(undefined),
  };
}
const sizes = (calls: unknown[][]) => calls.map(([ids]) => (ids as string[]).length);

async function selectAll() {
  await act(async () => hooks().s.selectAll());
  expect(hooks().s.count).toBe(TOTAL);
  expect(hooks().s.span).toMatchObject({ folder: "big", sort: "name", order: "asc" });
}

describe("commands over a span of a large folder", () => {
  test("moving to the trash asks the server for the ids a batch at a time and trashes each batch", async () => {
    const s = server();
    await selectAll();
    // One item left out with Ctrl
    await act(async () => hooks().s.choose(new Set(), { except: new Set(["b"]) }));
    expect(hooks().s.count).toBe(TOTAL - 1);
    await act(() => hooks().a.trash(hooks().s.picked));
    expect(s.selection).toHaveBeenCalledTimes(3);
    expect(s.selection.mock.calls[0]).toEqual(["big", { sort: "name", order: "asc", from: undefined, to: undefined, except: ["b"], after: undefined }]);
    expect(sizes(s.trash.mock.calls)).toEqual([1000, 1000, 499]);
    expect(s.trash.mock.calls.flatMap(([ids]) => ids)).not.toContain("b");
    // Progress shows while it goes, and a span can't be put back from the message
    expect(toast.loading).toHaveBeenCalled();
    expect(toast.success).toHaveBeenCalledWith("Moved 2,499 items to trash");
    expect(hooks().s.count).toBe(0);
  });

  test("items picked one by one go to the trash at once, and can be put back from the message", async () => {
    const s = server();
    await act(async () => hooks().s.setSelected(new Set(["a", "c"])));
    await act(() => hooks().a.trash(hooks().s.picked));
    expect(s.selection).not.toHaveBeenCalled();
    expect(s.trash.mock.calls).toEqual([[["a", "c"]]]);
    const [message, options] = toast.success.mock.calls[0] as [string, { action: { onClick(): void } }];
    expect(message).toBe("Moved to trash");
    options.action.onClick();
    expect(s.restore).toHaveBeenCalledWith(["a", "c"]);
  });

  test("a change that fails says why, and keeps the selection to try again", async () => {
    const s = server();
    s.trash.mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error("The disk is full"));
    await selectAll();
    await act(() => hooks().a.trash(hooks().s.picked));
    expect(s.trash).toHaveBeenCalledTimes(2);
    expect(toast.error).toHaveBeenCalledWith("The disk is full");
    expect(hooks().s.count).toBe(TOTAL);
  });

  test("deleting for good asks first, then trashes and deletes each batch", async () => {
    const s = server();
    await selectAll();
    confirm.mockResolvedValueOnce(false);
    await act(() => hooks().a.deleteForever(hooks().s.picked));
    expect(s.trash).not.toHaveBeenCalled();
    expect(s.deleteForever).not.toHaveBeenCalled();
    expect(hooks().s.count).toBe(TOTAL);

    await act(() => hooks().a.deleteForever(hooks().s.picked));
    expect(confirm.mock.calls[1][0]).toMatchObject({ title: "Permanently delete 2,500 items?", destructive: true, irreversible: true });
    expect(sizes(s.deleteForever.mock.calls)).toEqual([1000, 1000, 500]);
    // Each batch is in the trash before it is deleted
    for (const [i, [ids]] of s.deleteForever.mock.calls.entries()) {
      expect(s.trash.mock.calls[i][0]).toEqual(ids);
      expect(s.trash.mock.invocationCallOrder[i]).toBeLessThan(s.deleteForever.mock.invocationCallOrder[i]);
    }
    expect(toast.success).toHaveBeenCalledWith("Permanently deleted");
  });

  test("a span cut and pasted into another folder moves batch by batch, and the clipboard empties", async () => {
    const s = server();
    await selectAll();
    await act(async () => hooks().a.cut());
    await act(async () => hooks().go("dest"));
    expect(hooks().a.canPaste).toBe(true);
    await act(() => hooks().a.paste());
    expect(sizes(s.conflicts.mock.calls.map(([req]) => [req.ids]))).toEqual([1000, 1000, 500]);
    expect(s.move.mock.calls.map(([ids, dest]) => [ids.length, dest])).toEqual([
      [1000, "dest"],
      [1000, "dest"],
      [500, "dest"],
    ]);
    expect(toast.success).toHaveBeenCalledWith("Moved 2,500 items");
    expect(hooks().a.canPaste).toBe(false);
  });

  test("a span copied can be pasted again", async () => {
    const s = server();
    await selectAll();
    await act(async () => hooks().a.copy());
    await act(async () => hooks().go("dest"));
    await act(() => hooks().a.paste());
    expect(sizes(s.copy.mock.calls)).toEqual([1000, 1000, 500]);
    expect(s.move).not.toHaveBeenCalled();
    expect(hooks().a.canPaste).toBe(true);
  });

  test("moving a span with Move to goes batch by batch, never into a folder it holds", async () => {
    const s = server();
    await selectAll();
    const moved = await act(() => hooks().a.transfer("move", hooks().s.picked, "n5", (n) => `Moved ${n}`, "Couldn't move"));
    expect(moved).toBe(true);
    expect(s.move.mock.calls.flatMap(([ids]) => ids)).not.toContain("n5");
    expect(sizes(s.move.mock.calls)).toEqual([999, 1000, 500]);
    expect(toast.success).toHaveBeenCalledWith("Moved 2499");
    expect(hooks().s.count).toBe(0);
  });

  test("adding a span to favorites goes batch by batch", async () => {
    const s = server();
    await selectAll();
    await act(() => hooks().a.toggleFavorite());
    expect(s.setFavorite.mock.calls.map(([ids, on]) => [ids.length, on])).toEqual([
      [1000, true],
      [1000, true],
      [500, true],
    ]);
    expect(toast.success).toHaveBeenCalledWith("Added to favorites");
  });
});
