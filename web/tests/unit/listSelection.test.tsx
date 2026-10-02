// Selecting in the smaller lists (lib/listSelection.ts: spaces, the control panel, storage locations, the admin tables)
// with the mouse and the keyboard, like the file list
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import { useSelectableList } from "@/lib/listSelection";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const NAMES = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta"];

type Options = { multi?: boolean; grid?: number; split?: number; hidden?: (n: string) => boolean; onOpen?: (n: string, newTab: boolean) => void };
const state: { selected: Set<string>; anchor: string | null } = { selected: new Set(), anchor: null };

/** The items in one list (or two, the second from `split` on), as tiles in a grid of `grid` columns or as rows */
function List({ multi = true, grid, split, hidden, onOpen }: Options) {
  const [selected, setSelected] = useState(new Set<string>());
  const sel = useSelectableList({ items: NAMES, keyOf: (n) => n, selected, onSelect: setSelected, multi, onOpen, nameOf: (n) => n, hidden });
  state.selected = selected;
  state.anchor = sel.anchor;
  const style = grid ? { display: "grid", gridTemplateColumns: Array(grid).fill("1fr").join(" ") } : undefined;
  const lists = split ? [NAMES.slice(0, split), NAMES.slice(split)] : [NAMES];
  return (
    <div {...sel.scopeProps}>
      {lists.map((names, i) => (
        <div key={i} {...sel.listProps(grid ? "listbox" : "grid", `List ${i}`)} style={style}>
          {names.map((n) => (
            <div key={n} {...sel.itemProps(n)}>
              {n}
              <input aria-label={`Rename ${n}`} />
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

let root: Root | null = null;
let container: HTMLElement;
function mount(o: Options = {}) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root!.render(<List {...o} />));
}
afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container.remove();
});

const item = (name: string) => container.querySelector<HTMLElement>(`[data-item-key="${name}"]`)!;
const selected = () => NAMES.filter((n) => state.selected.has(n));
const focused = () => (document.activeElement as HTMLElement | null)?.dataset.itemKey;
type Mods = { ctrlKey?: boolean; shiftKey?: boolean; metaKey?: boolean; altKey?: boolean };
function click(name: string, mods: Mods = {}) {
  act(() => void item(name).dispatchEvent(new MouseEvent("click", { bubbles: true, ...mods })));
}
function key(name: string, k: string, mods: Mods & { repeat?: boolean } = {}, target: HTMLElement = item(name)) {
  const event = new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...mods });
  act(() => void target.dispatchEvent(event));
  return event.defaultPrevented;
}

describe("selecting with the mouse", () => {
  test("a click selects one item; Ctrl adds or removes; Shift selects from the last item clicked", () => {
    mount();
    click("Beta");
    expect(selected()).toEqual(["Beta"]);
    expect(state.anchor).toBe("Beta");
    click("Delta", { ctrlKey: true });
    expect(selected()).toEqual(["Beta", "Delta"]);
    click("Beta", { metaKey: true });
    expect(selected()).toEqual(["Delta"]);
    // The range starts at the last item clicked without Shift (with Ctrl too), and replaces what was selected
    click("Zeta", { shiftKey: true });
    expect(selected()).toEqual(["Beta", "Gamma", "Delta", "Epsilon", "Zeta"]);
    click("Delta", { shiftKey: true });
    expect(selected()).toEqual(["Beta", "Gamma", "Delta"]);
    expect(state.anchor).toBe("Beta");
    // With Ctrl too, the range is added to what is selected
    click("Alpha");
    click("Gamma", { ctrlKey: true });
    click("Epsilon", { ctrlKey: true, shiftKey: true });
    expect(selected()).toEqual(["Alpha", "Gamma", "Delta", "Epsilon"]);
  });

  test("a list that selects one item ignores Ctrl and Shift", () => {
    mount({ multi: false });
    click("Beta");
    click("Delta", { ctrlKey: true });
    click("Zeta", { shiftKey: true });
    expect(selected()).toEqual(["Zeta"]);
    expect(item("Zeta").closest("[role]")!.getAttribute("aria-multiselectable")).toBeNull();
  });

  test("double and middle clicks open, and a right click selects only an item not selected", () => {
    const onOpen = vi.fn<(n: string, newTab: boolean) => void>();
    mount({ onOpen });
    act(() => void item("Gamma").dispatchEvent(new MouseEvent("dblclick", { bubbles: true })));
    act(() => void item("Delta").dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 1 })));
    act(() => void item("Delta").dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 2 })));
    expect(onOpen.mock.calls).toEqual([
      ["Gamma", false],
      ["Delta", true],
    ]);
    click("Alpha");
    click("Beta", { ctrlKey: true });
    act(() => void item("Beta").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true })));
    expect(selected()).toEqual(["Alpha", "Beta"]);
    act(() => void item("Eta").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true })));
    expect(selected()).toEqual(["Eta"]);
  });
});

describe("selecting with the keyboard", () => {
  test("one item is a Tab stop: the first selected one, else the first that can take the focus", () => {
    mount({ hidden: (n) => n === "Alpha" });
    const stops = () => NAMES.filter((n) => item(n).tabIndex === 0);
    expect(stops()).toEqual(["Beta"]);
    click("Delta");
    click("Zeta", { ctrlKey: true });
    expect(stops()).toEqual(["Delta"]);
    expect(item("Delta").getAttribute("aria-selected")).toBe("true");
    expect(item("Gamma").getAttribute("aria-selected")).toBe("false");
  });

  test("the arrows, Home and End move the selection; with Shift they extend it, with Ctrl only the focus moves", () => {
    mount();
    click("Beta");
    expect(key("Beta", "ArrowDown")).toBe(true);
    expect([selected(), focused()]).toEqual([["Gamma"], "Gamma"]);
    key("Gamma", "ArrowDown", { shiftKey: true });
    key("Delta", "ArrowDown", { shiftKey: true });
    expect(selected()).toEqual(["Gamma", "Delta", "Epsilon"]);
    // Ctrl moves the focus only; Ctrl+Space then adds the item there
    key("Epsilon", "ArrowDown", { ctrlKey: true });
    key("Zeta", "ArrowDown", { ctrlKey: true });
    expect([selected(), focused()]).toEqual([["Gamma", "Delta", "Epsilon"], "Eta"]);
    key("Eta", " ", { ctrlKey: true });
    expect(selected()).toEqual(["Gamma", "Delta", "Epsilon", "Eta"]);
    key("Eta", " ", { ctrlKey: true });
    expect(selected()).toEqual(["Gamma", "Delta", "Epsilon"]);
    // At the end there is nowhere further to go
    expect(key("Eta", "ArrowDown")).toBe(true);
    expect(focused()).toBe("Eta");
    key("Eta", "Home");
    expect([selected(), focused()]).toEqual([["Alpha"], "Alpha"]);
    key("Alpha", "End", { shiftKey: true });
    expect(selected()).toEqual(NAMES);
    key("Eta", " ");
    expect(selected()).toEqual(["Eta"]);
    // Left and Right only move between tiles
    expect(key("Eta", "ArrowLeft")).toBe(false);
  });

  test("Ctrl+A selects everything, Esc nothing, Enter opens (with Ctrl in a new tab) once per press", () => {
    const onOpen = vi.fn<(n: string, newTab: boolean) => void>();
    mount({ onOpen });
    click("Gamma");
    key("Gamma", "a", { ctrlKey: true });
    expect(selected()).toEqual(NAMES);
    key("Gamma", "Escape");
    expect(selected()).toEqual([]);
    // Esc with nothing selected is left to the page (to close a dialog, say)
    expect(key("Gamma", "Escape")).toBe(false);
    key("Gamma", "Enter");
    key("Gamma", "Enter", { ctrlKey: true });
    key("Gamma", "Enter", { repeat: true });
    expect(onOpen.mock.calls).toEqual([
      ["Gamma", false],
      ["Gamma", true],
    ]);
  });

  test("keys typed in a control inside an item, or with Alt, are left alone", () => {
    mount();
    click("Beta");
    const input = item("Beta").querySelector("input")!;
    expect(key("Beta", "ArrowDown", {}, input)).toBe(false);
    expect(key("Beta", "ArrowDown", { altKey: true })).toBe(false);
    expect(selected()).toEqual(["Beta"]);
  });

  test("typing the start of a name goes to it, and the same letter again goes on to the next", () => {
    vi.useFakeTimers();
    try {
      mount();
      click("Alpha");
      key("Alpha", "d");
      expect([selected(), focused()]).toEqual([["Delta"], "Delta"]);
      vi.advanceTimersByTime(1500);
      key("Delta", "e");
      expect(selected()).toEqual(["Epsilon"]);
      key("Epsilon", "e");
      expect(selected()).toEqual(["Eta"]);
      vi.advanceTimersByTime(1500);
      // Letters typed together spell the start of a name
      key("Eta", "z");
      key("Zeta", "e");
      expect(selected()).toEqual(["Zeta"]);
      vi.advanceTimersByTime(1500);
      // Nothing starts with it: the selection stays
      expect(key("Zeta", "q")).toBe(true);
      expect(selected()).toEqual(["Zeta"]);
    } finally {
      vi.useRealTimers();
    }
  });

  test("in a grid of tiles the arrows go up and down a row, and on into the next list", () => {
    // Two lists of tiles three to a row: Alpha Beta Gamma / Delta, then Epsilon Zeta Eta
    mount({ grid: 3, split: 4 });
    click("Beta");
    key("Beta", "ArrowRight");
    expect(selected()).toEqual(["Gamma"]);
    // Below Gamma is empty, but the shorter last row has Delta
    key("Gamma", "ArrowDown");
    expect(selected()).toEqual(["Delta"]);
    // From the last row, down goes into the next list, in the same column
    key("Delta", "ArrowDown");
    expect(selected()).toEqual(["Epsilon"]);
    key("Epsilon", "ArrowRight");
    key("Zeta", "ArrowUp");
    // Up from the next list: the column in the last row above, or its last item
    expect(selected()).toEqual(["Delta"]);
    key("Delta", "ArrowUp");
    expect(selected()).toEqual(["Alpha"]);
    expect(key("Alpha", "ArrowUp")).toBe(true);
    expect(selected()).toEqual(["Alpha"]);
    key("Alpha", "ArrowLeft");
    expect(selected()).toEqual(["Alpha"]);
  });
});
