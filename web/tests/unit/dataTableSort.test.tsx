// A table whose headers sort: a header with a sort key is a button, the sorted one says which way, and the others are plain
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { DataTable, type Column } from "@/components/DataTable";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

interface Row {
  id: string;
  name: string;
  size: number;
}
const rows: Row[] = [
  { id: "1", name: "b", size: 2 },
  { id: "2", name: "a", size: 1 },
];
const columns: Column<Row>[] = [
  { header: "Name", sortKey: "name", cell: (r) => r.name },
  { header: "Size", sortKey: "size", cell: (r) => r.size },
  { header: "Notes", cell: () => "" },
];

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function show(extra: { sort?: { key: string; order: "asc" | "desc" }; onSort?: (key: string) => void }) {
  act(() => root.render(<DataTable label="Things" rows={rows} rowKey={(r) => r.id} columns={columns} selectedKey={null} onSelect={() => {}} menu={() => null} {...extra} />));
}
const headers = () => [...host.querySelectorAll<HTMLElement>("th")];

test("the headers with a sort key are buttons that name their key, and the sorted one says which way", () => {
  const onSort = vi.fn<(key: string) => void>();
  show({ sort: { key: "size", order: "desc" }, onSort });
  const [name, size, notes] = headers();
  expect(name.querySelector("button")).not.toBeNull();
  expect(notes.querySelector("button")).toBeNull();
  expect([name.getAttribute("aria-sort"), size.getAttribute("aria-sort"), notes.getAttribute("aria-sort")]).toEqual([null, "descending", null]);
  expect(size.querySelector("svg")).not.toBeNull();
  expect(name.querySelector("svg")).toBeNull();
  act(() => name.querySelector("button")!.click());
  act(() => size.querySelector("button")!.click());
  expect(onSort.mock.calls).toEqual([["name"], ["size"]]);
});

test("ascending is told too, and a table that is not asked to sort keeps plain headers", () => {
  show({ sort: { key: "name", order: "asc" }, onSort: () => {} });
  expect(headers()[0].getAttribute("aria-sort")).toBe("ascending");
  show({});
  expect(host.querySelectorAll("th button")).toHaveLength(0);
  expect(headers().map((h) => h.textContent)).toEqual(["Name", "Size", "Notes"]);
});
