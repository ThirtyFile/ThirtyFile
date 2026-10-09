// Quick look, the Mac style's preview window (components/style/mac/quickLook.tsx): which items it goes through, its key,
// the arrows and the selection following them
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import type { Item } from "@/components/fileList/layout";
import { macKit } from "@/components/style/mac";
import { QuickLook, lookItems } from "@/components/style/mac/quickLook";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// The previews every style shares are tested on their own
vi.mock("@/components/FileViewer", () => ({ FileViewer: ({ node }: { node: { name: string } }) => <p data-preview>{node.name}</p> }));

const item = (name: string) => ({ id: name, name, kind: name.includes(".") ? "file" : "folder", size: 0, updated_at: 0 }) as Item;
const LIST = ["Drafts", "a.txt", "b.png", "c.pdf"].map(item);

test("goes through the items selected when there are several, else through the whole list", () => {
  expect(lookItems(LIST, new Set(["b.png"])).map((n) => n.id)).toEqual(["Drafts", "a.txt", "b.png", "c.pdf"]);
  expect(lookItems([LIST[0], undefined, LIST[2], LIST[3]], new Set(["c.pdf", "Drafts"])).map((n) => n.id)).toEqual(["Drafts", "c.pdf"]);
});

describe("the window", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
  });

  /** The explorer's state, as far as Quick look uses it: the list, and a selection that follows */
  function state(selected: string[], shown: readonly (Item | undefined)[] = LIST, total = shown.length) {
    return {
      kit: macKit,
      shown,
      total,
      selected: new Set(selected),
      anchor: selected[0] ?? null,
      count: selected.length,
      selectedNodes: shown.filter((n): n is Item => !!n && selected.includes(n.id)),
      setSelected: vi.fn<(next: Set<string>) => void>(),
      setAnchor: vi.fn<(id: string) => void>(),
      listNav: { current: { show: vi.fn<(id: string, focus: boolean) => void>(), scrollTo: vi.fn<(index: number) => void>() } },
    };
  }
  const a = { open: vi.fn<(n: Item) => void>() };
  /** Shows Quick look's part of the explorer with the state `s` (again, when it changed) */
  function show(s: ReturnType<typeof state>) {
    if (!root) {
      const el = document.createElement("div");
      document.body.append(el);
      root = createRoot(el);
    }
    act(() => root!.render(<QuickLook p={{} as ExplorerProps} s={s as unknown as ExplorerState} a={a as unknown as ExplorerActions} />));
  }
  function render(selected: string[], shown?: readonly (Item | undefined)[], total?: number) {
    a.open.mockClear();
    const s = state(selected, shown, total);
    show(s);
    return { s, a };
  }
  /** A key pressed where the focus is (in the window once it is open) */
  const key = (k: string) => act(() => void (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true })));
  const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');

  test("Space opens it on the item selected; the arrows go to the next item and select it; Space closes it", () => {
    const { s, a } = render(["a.txt"]);
    key(" ");
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: a.txt");
    expect(dialog()!.querySelector("[data-preview]")!.textContent).toBe("a.txt");
    expect(dialog()!.textContent).toContain("2 of 4");
    key("ArrowDown");
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: b.png");
    expect(s.setSelected).toHaveBeenLastCalledWith(new Set(["b.png"]));
    expect(s.listNav.current.show).toHaveBeenLastCalledWith("b.png", false);
    key("ArrowLeft");
    key("ArrowLeft");
    // A folder has no preview: its card
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: Drafts");
    expect(dialog()!.querySelector("[data-preview]")).toBeNull();
    key(" ");
    expect(dialog()).toBeNull();
    expect(a.open).not.toHaveBeenCalled();
  });

  test("with several items selected, it goes through those and leaves the selection alone; Esc closes it", () => {
    const { s } = render(["a.txt", "c.pdf"]);
    key(" ");
    expect(dialog()!.textContent).toContain("1 of 2");
    key("ArrowRight");
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: c.pdf");
    key("ArrowRight");
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: c.pdf");
    expect(s.setSelected).not.toHaveBeenCalled();
    key("Escape");
    expect(dialog()).toBeNull();
  });

  test("Open opens the item shown, as a double click does", () => {
    const { a } = render(["c.pdf"]);
    key(" ");
    act(() => [...dialog()!.querySelectorAll("button")].find((b) => b.textContent!.trim() === "Open")!.click());
    expect(a.open).toHaveBeenCalledWith(expect.objectContaining({ id: "c.pdf" }));
    expect(dialog()).toBeNull();
  });

  test("nothing opens with nothing selected", () => {
    render([]);
    key(" ");
    expect(dialog()).toBeNull();
  });

  test("in a large folder it counts every item, and going to one not loaded yet loads its part first", () => {
    const shown = [LIST[1], LIST[2], undefined, undefined];
    const { s } = render(["b.png"], shown, 1000);
    key(" ");
    expect(dialog()!.textContent).toContain("2 of 1,000");
    key("ArrowDown");
    // Not loaded: the list goes there, and Quick look waits for it
    expect(s.listNav.current.scrollTo).toHaveBeenCalledWith(2);
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: b.png");
    const loaded = state(["b.png"], [LIST[1], LIST[2], LIST[3], undefined], 1000);
    show(loaded);
    expect(dialog()!.getAttribute("aria-label")).toBe("Quick look: c.pdf");
    expect(dialog()!.textContent).toContain("3 of 1,000");
    expect(loaded.setSelected).toHaveBeenLastCalledWith(new Set(["c.pdf"]));
  });

  test("it closes when nothing is selected any more, and doesn't come back by itself", () => {
    render(["a.txt"]);
    key(" ");
    expect(dialog()).not.toBeNull();
    // Another folder opened: the selection is cleared
    show(state([]));
    expect(dialog()).toBeNull();
    show(state(["b.png"]));
    expect(dialog()).toBeNull();
  });

  test("Space on one of its buttons presses the button, and a click beside it closes it", () => {
    render(["a.txt"]);
    key(" ");
    const next = [...dialog()!.querySelectorAll("button")].find((b) => b.getAttribute("aria-label") === "Next item")!;
    act(() => next.focus());
    key(" ");
    expect(dialog()).not.toBeNull();
    act(() => void document.querySelector("[aria-hidden].fixed.inset-0")!.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })));
    expect(dialog()).toBeNull();
  });
});
