// Dragging items onto folders (lib/dnd.ts): what a drop carries, moving or copying, and a large folder's span
import { beforeEach, describe, expect, test, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";
import type { DragEvent } from "react";
import type { FolderSpan } from "@/lib/span";

const transferItems = vi.hoisted(() => vi.fn<(...args: unknown[]) => Promise<void>>(() => Promise.resolve()));
vi.mock("@/lib/transfer", () => ({ transferItems }));

const { DRAG_MIME, carriesFiles, carriesItems, dropEffect, dropItems, droppedIds, startDrag } = await import("@/lib/dnd");

/** A DataTransfer holding the given types (only what the code reads) */
function transfer(data: Record<string, string>, files = false): DataTransfer {
  const store = { ...data };
  return {
    types: [...Object.keys(store), ...(files ? ["Files"] : [])],
    getData: (type: string) => store[type] ?? "",
    setData: (type: string, value: string) => void (store[type] = value),
    effectAllowed: "none",
  } as unknown as DataTransfer;
}

const drag = (dt: DataTransfer, keys: { ctrlKey?: boolean; altKey?: boolean } = {}) =>
  ({ dataTransfer: dt, ctrlKey: false, altKey: false, ...keys, currentTarget: document.createElement("div") }) as unknown as DragEvent;

beforeEach(() => transferItems.mockClear());

describe("drag and drop", () => {
  test("a drop carries item ids only when they are a list of strings", () => {
    expect(droppedIds(transfer({ [DRAG_MIME]: JSON.stringify(["a", "b"]) }))).toEqual(["a", "b"]);
    expect(droppedIds(transfer({ [DRAG_MIME]: JSON.stringify([1, 2]) }))).toBeNull();
    expect(droppedIds(transfer({ [DRAG_MIME]: "not json" }))).toBeNull();
    expect(droppedIds(transfer({ "text/plain": "a" }))).toBeNull();
    expect(carriesItems(transfer({ [DRAG_MIME]: "[]" }))).toBe(true);
    expect(carriesFiles(transfer({}, true))).toBe(true);
  });

  test("items dragged move, with Ctrl they're copied, and files from the computer are always copied", () => {
    const items = transfer({ [DRAG_MIME]: "[]" });
    expect(dropEffect(drag(items))).toBe("move");
    expect(dropEffect(drag(items, { ctrlKey: true }))).toBe("copy");
    expect(dropEffect(drag(transfer({}, true)))).toBe("copy");
  });

  test("dropping items moves them, leaving out the folder itself; with Ctrl they're copied", async () => {
    const qc = new QueryClient();
    await dropItems(qc, ["a", "folder", "b"], { id: "folder", name: "Reports" }, false);
    expect(transferItems).toHaveBeenCalledTimes(1);
    const [, op, what, dest] = transferItems.mock.calls[0];
    expect([op, what, dest]).toEqual(["move", { ids: ["a", "b"], span: null, count: 2 }, "folder"]);

    await dropItems(qc, ["a"], { id: "other", name: "Other" }, true);
    expect(transferItems.mock.calls[1][1]).toBe("copy");

    // A folder dropped onto itself does nothing
    await dropItems(qc, ["folder"], { id: "folder", name: "Reports" }, false);
    expect(transferItems).toHaveBeenCalledTimes(2);
  });

  test("a large folder's span goes with the items dragged from this page, and never into that folder", async () => {
    const qc = new QueryClient();
    const dt = transfer({});
    const span: FolderSpan = { folder: "big", sort: "name", order: "asc", count: 1000, except: new Set() };
    startDrag(drag(dt), ["x"], [{ id: "x", parent_id: "big" }], span);
    expect(droppedIds(dt)).toEqual(["x"]);
    await dropItems(qc, ["x"], { id: "dest", name: "Dest" }, false);
    expect(transferItems.mock.calls[0][2]).toEqual({ ids: ["x"], span, count: 1001 });
    // Dropped into the folder it came from: nothing to move
    await dropItems(qc, ["x"], { id: "big", name: "Big" }, false);
    expect(transferItems).toHaveBeenCalledTimes(1);
  });
});
