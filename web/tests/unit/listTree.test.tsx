// Folders that expand in place in the List view (components/fileList/listTree.ts): the rows, which parts of each list
// the rows in view need, and the list with its triangles and keys
import { act, useMemo, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { FileSource, Node } from "@/api";
import { FileList } from "@/components/FileList";
import { flatten, partsFor, shownByFolder, type Branch, type ListTreeView } from "@/components/fileList/listTree";
import type { ListSpan } from "@/lib/span";
import { WINDOW } from "@/lib/windows";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const node = (name: string, parent = "home"): Node => ({
  id: name,
  parent_id: parent,
  kind: name.includes(".") ? "file" : "folder",
  name,
  size: 1,
  mime: "",
  created_at: 0,
  updated_at: 0,
  trashed_at: null,
  drive_id: "d",
  owner_name: "",
  is_favorite: false,
});
const BASE = ["Archive", "Projects", "notes.txt"].map((n) => node(n));
const branch = (parent: string, names: (string | undefined)[]): Branch => ({ at: names.map((n) => (n ? node(n, parent) : undefined)), total: names.length });

describe("the rows", () => {
  test("an expanded folder's items follow it, one level deeper, and theirs too", () => {
    const branches = new Map([
      ["Projects", branch("Projects", ["Design", "plan.md"])],
      ["Design", branch("Design", ["logo.svg"])],
    ]);
    const rows = flatten(BASE, new Set(["Projects", "Design"]), branches);
    expect(rows.map((r) => [r.item?.name, r.depth, r.folder, r.pos])).toEqual([
      ["Archive", 0, null, 0],
      ["Projects", 0, null, 1],
      ["Design", 1, "Projects", 0],
      ["logo.svg", 2, "Design", 0],
      ["plan.md", 1, "Projects", 1],
      ["notes.txt", 0, null, 2],
    ]);
    // A folder expanded inside a collapsed one stays hidden
    expect(flatten(BASE, new Set(["Design"]), branches).map((r) => r.item?.name)).toEqual(["Archive", "Projects", "notes.txt"]);
  });

  test("a folder whose items are loading has a row saying so, and a large one has a row for each item not loaded", () => {
    expect(flatten(BASE, new Set(["Archive"]), new Map()).map((r) => [r.item?.name, r.depth, r.folder])).toEqual([
      ["Archive", 0, null],
      [undefined, 1, "Archive"],
      ["Projects", 0, null],
      ["notes.txt", 0, null],
    ]);
    // Parts not loaded are holes in the folder's list
    const big: Branch = { at: Object.assign(new Array(4), { 0: node("a.txt", "Archive") }), total: 4 };
    expect(flatten(BASE.slice(0, 1), new Set(["Archive"]), new Map([["Archive", big]])).map((r) => r.item?.name ?? "…")).toEqual(["Archive", "a.txt", "…", "…", "…"]);
  });

  test("the rows in view say which positions of each list they show; the list itself keeps the folder they are in", () => {
    const rows = flatten(BASE, new Set(["Projects"]), new Map([["Projects", branch("Projects", ["a.txt", "b.txt", "c.txt"])]]));
    expect([...shownByFolder(rows, 0, 2)]).toEqual([
      [null, [0, 1]],
      ["Projects", [0, 0]],
    ]);
    expect([...shownByFolder(rows, 3, 4)]).toEqual([
      ["Projects", [1, 2]],
      [null, [1, 1]],
    ]);
  });

  test("the parts to load are those in view, with one more each way, within the list", () => {
    expect(partsFor(-1, undefined)).toEqual([0]);
    expect(partsFor(3 * WINDOW, undefined)).toEqual([0, WINDOW]);
    expect(partsFor(3 * WINDOW, [2 * WINDOW + 5, 2 * WINDOW + 40])).toEqual([WINDOW, 2 * WINDOW]);
  });
});

describe("the List view with folders that expand", () => {
  const source: FileSource = { contentUrl: () => "", viewUrl: () => "/view", thumbUrl: () => "", downloadLink: () => Promise.resolve("") };
  const BRANCHES = new Map([["Projects", { at: [node("Design", "Projects"), undefined, node("plan.md", "Projects")], total: 3 }]]);

  function Harness() {
    const [open, setOpen] = useState<string[]>([]);
    const [selected, setSelected] = useState(new Set<string>());
    const [anchor, setAnchor] = useState<string | null>(null);
    const [span, setSpan] = useState<ListSpan | null>(null);
    const rows = useMemo(() => flatten(BASE, new Set(open), BRANCHES), [open]);
    const tree: ListTreeView = {
      expanded: open.length > 0,
      depth: (i) => rows[i]?.depth ?? 0,
      isOpen: (id) => open.includes(id),
      toggle: (id, on) => setOpen((o) => (on ? [...o, id] : o.filter((x) => x !== id))),
    };
    seen.span = span;
    return (
      <FileList
        items={rows.map((r) => r.item)}
        view="list"
        tree={tree}
        source={source}
        selected={selected}
        anchor={anchor}
        span={span}
        onSelect={(s, a, sp) => {
          setSelected(s);
          setAnchor(a ?? null);
          setSpan(sp ?? null);
        }}
        onOpen={vi.fn<(n: Node) => void>()}
      />
    );
  }
  const seen: { span: ListSpan | null } = { span: null };

  let root: Root | null = null;
  let container: HTMLElement;
  function mount() {
    container = document.createElement("div");
    container.style.overflowY = "auto";
    Object.defineProperty(container, "offsetHeight", { value: 600 });
    Object.defineProperty(container, "offsetWidth", { value: 1000 });
    document.body.append(container);
    root = createRoot(container);
    act(() => root!.render(<Harness />));
  }
  afterEach(() => {
    act(() => root?.unmount());
    container.remove();
    root = null;
  });
  const row = (id: string) => container.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`)!;
  const rows = () => [...container.querySelectorAll("tbody tr")].map((r) => [r.getAttribute("data-node-id") ?? "…", r.getAttribute("aria-level"), r.getAttribute("aria-expanded")]);
  const key = (el: HTMLElement, k: string, mods: KeyboardEventInit = {}) =>
    act(() => void el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...mods })));
  const click = (el: Element, mods: MouseEventInit = {}) => act(() => void el.dispatchEvent(new MouseEvent("click", { bubbles: true, ...mods })));
  const selectedNames = () => [...container.querySelectorAll('[aria-selected="true"]')].map((r) => r.getAttribute("data-node-id"));

  test("is a tree grid: rows say how deep they are, and folders whether they are expanded", () => {
    mount();
    expect(container.querySelector("table")!.getAttribute("role")).toBe("treegrid");
    expect(rows()).toEqual([
      ["Archive", "1", "false"],
      ["Projects", "1", "false"],
      ["notes.txt", "1", null],
    ]);
  });

  test("a folder's triangle expands it in place, and collapses it, without changing the selection", () => {
    mount();
    click(row("Projects").querySelector("[data-disclosure]")!);
    expect(rows()).toEqual([
      ["Archive", "1", "false"],
      ["Projects", "1", "true"],
      ["Design", "2", "false"],
      ["…", null, null],
      ["plan.md", "2", null],
      ["notes.txt", "1", null],
    ]);
    expect(selectedNames()).toEqual([]);
    click(row("Projects").querySelector("[data-disclosure]")!);
    expect(rows()).toHaveLength(3);
  });

  test("→ expands a folder, then goes into it; ← goes back to the folder, then collapses it", () => {
    mount();
    click(row("Projects"));
    key(row("Projects"), "ArrowRight");
    expect(row("Projects").getAttribute("aria-expanded")).toBe("true");
    expect(selectedNames()).toEqual(["Projects"]);
    key(row("Projects"), "ArrowRight");
    expect(selectedNames()).toEqual(["Design"]);
    expect(document.activeElement).toBe(row("Design"));
    key(row("Design"), "ArrowLeft");
    expect(selectedNames()).toEqual(["Projects"]);
    key(row("Projects"), "ArrowLeft");
    expect(row("Projects").getAttribute("aria-expanded")).toBe("false");
  });

  test("a range across items not loaded selects those loaded, not a span of the list", () => {
    mount();
    click(row("Projects").querySelector("[data-disclosure]")!);
    click(row("Design"));
    click(row("notes.txt"), { shiftKey: true });
    expect(selectedNames()).toEqual(["Design", "plan.md", "notes.txt"]);
    expect(seen.span).toBeNull();
  });
});
