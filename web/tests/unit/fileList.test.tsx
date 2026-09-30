// The file list (components/FileList.tsx) in the page: selecting with the mouse and the keyboard, and opening items
import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { FileSource, Node } from "@/api";
import { FileList, type ListNav } from "@/components/FileList";
import type { ListSpan } from "@/lib/span";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const node = (name: string, kind: "file" | "folder" = "file"): Node => ({
  id: name,
  parent_id: "folder",
  kind,
  name,
  size: 1024,
  mime: kind === "folder" ? "" : "text/plain",
  created_at: 1_700_000_000,
  updated_at: 1_700_000_000,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "admin",
  is_favorite: false,
});
const ITEMS = ["Budget.xlsx", "Contracts", "Minutes.docx", "Notes.txt", "Photos"].map((n) => node(n, n.includes(".") ? "file" : "folder"));
const source: FileSource = { contentUrl: () => "", thumbUrl: () => "", downloadLink: () => Promise.resolve("") };

/** The list with its selection kept like the explorer keeps it */
function Harness({ onOpen, navRef }: { onOpen(n: Node, byKey?: boolean): void; navRef?: { current: ListNav | null } }) {
  const [selected, setSelected] = useState(new Set<string>());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [span, setSpan] = useState<ListSpan | null>(null);
  const nav = useRef<ListNav | null>(null);
  if (navRef) navRef.current = nav.current;
  return (
    <FileList
      items={ITEMS}
      view="list"
      source={source}
      selected={selected}
      anchor={anchor}
      span={span}
      onSelect={(s, a, sp) => {
        setSelected(s);
        setAnchor(a ?? null);
        setSpan(sp ?? null);
      }}
      onOpen={onOpen}
      navRef={navRef ?? nav}
    />
  );
}

let root: Root | null = null;
let container: HTMLElement;
/** The list in a scrolling box 600 pixels high, which the list's rows are laid out in (the test page has no layout) */
function mount(props: Parameters<typeof Harness>[0]) {
  container = document.createElement("div");
  container.style.overflowY = "auto";
  Object.defineProperty(container, "offsetHeight", { value: 600 });
  Object.defineProperty(container, "offsetWidth", { value: 1000 });
  document.body.append(container);
  root = createRoot(container);
  act(() => root!.render(<Harness {...props} />));
}
afterEach(() => {
  act(() => root?.unmount());
  container.remove();
  root = null;
});

const row = (name: string) => container.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(name)}"]`)!;
const selectedNames = () => [...container.querySelectorAll('[role="row"][aria-selected="true"]')].map((r) => r.getAttribute("data-node-id"));
const key = (el: HTMLElement, k: string, mods: KeyboardEventInit = {}) =>
  act(() => void el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...mods })));
const click = (el: HTMLElement, mods: MouseEventInit = {}) => act(() => void el.dispatchEvent(new MouseEvent("click", { bubbles: true, ...mods })));

describe("file list", () => {
  test("shows every item as a row of a grid, in order", () => {
    mount({ onOpen: vi.fn<(n: Node) => void>() });
    const grid = container.querySelector('[role="grid"]')!;
    expect(grid.getAttribute("aria-label")).toBe("Items");
    expect([...container.querySelectorAll("[data-node-id]")].map((r) => r.getAttribute("data-node-id"))).toEqual(ITEMS.map((n) => n.id));
    // One Tab stop: the first item until something is selected
    expect(row("Budget.xlsx").tabIndex).toBe(0);
    expect(row("Contracts").tabIndex).toBe(-1);
  });

  test("a click selects one item, Ctrl+click adds or removes one, Shift+click selects the range from the last one clicked", () => {
    mount({ onOpen: vi.fn<(n: Node) => void>() });
    click(row("Contracts"));
    expect(selectedNames()).toEqual(["Contracts"]);
    click(row("Notes.txt"), { ctrlKey: true });
    expect(selectedNames()).toEqual(["Contracts", "Notes.txt"]);
    click(row("Notes.txt"), { ctrlKey: true });
    expect(selectedNames()).toEqual(["Contracts"]);
    // The range starts at the item clicked last, as in File Explorer
    click(row("Photos"), { shiftKey: true });
    expect(selectedNames()).toEqual(["Notes.txt", "Photos"]);
    click(row("Contracts"));
    click(row("Notes.txt"), { shiftKey: true });
    expect(selectedNames()).toEqual(["Contracts", "Minutes.docx", "Notes.txt"]);
  });

  test("the arrows move the selection, Shift extends it, Ctrl moves only the focus and Ctrl+Space picks", () => {
    mount({ onOpen: vi.fn<(n: Node) => void>() });
    click(row("Budget.xlsx"));
    key(row("Budget.xlsx"), "ArrowDown");
    expect(selectedNames()).toEqual(["Contracts"]);
    expect(document.activeElement).toBe(row("Contracts"));
    key(row("Contracts"), "ArrowDown", { shiftKey: true });
    expect(selectedNames()).toEqual(["Contracts", "Minutes.docx"]);
    key(row("Minutes.docx"), "ArrowDown", { ctrlKey: true });
    expect(selectedNames()).toEqual(["Contracts", "Minutes.docx"]);
    expect(document.activeElement).toBe(row("Notes.txt"));
    key(row("Notes.txt"), " ", { ctrlKey: true });
    expect(selectedNames()).toEqual(["Contracts", "Minutes.docx", "Notes.txt"]);
    key(row("Notes.txt"), "Home");
    expect(selectedNames()).toEqual(["Budget.xlsx"]);
    key(row("Budget.xlsx"), "End");
    expect(selectedNames()).toEqual(["Photos"]);
  });

  test("Enter opens the item, and a double click too", () => {
    const onOpen = vi.fn<(n: Node, byKey?: boolean) => void>();
    mount({ onOpen });
    click(row("Contracts"));
    key(row("Contracts"), "Enter");
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ id: "Contracts" }), true);
    act(() => void row("Notes.txt").dispatchEvent(new MouseEvent("dblclick", { bubbles: true })));
    expect(onOpen).toHaveBeenLastCalledWith(expect.objectContaining({ id: "Notes.txt" }));
  });

  test("typing letters goes to the next item starting with them", () => {
    const navRef: { current: ListNav | null } = { current: null };
    mount({ onOpen: vi.fn<(n: Node) => void>(), navRef });
    act(() => navRef.current!.typeAhead("n"));
    expect(selectedNames()).toEqual(["Notes.txt"]);
    act(() => navRef.current!.typeAhead("p"));
    // "np" matches nothing: the selection stays
    expect(selectedNames()).toEqual(["Notes.txt"]);
  });
});
