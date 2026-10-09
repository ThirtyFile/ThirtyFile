// The Mac style's Gallery view (components/style/mac/gallery.tsx): which item it shows, the strip of thumbnails (the
// file list laid out across), the keys, the preview, what is announced, and the details panel that can be hidden
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { FileSource } from "@/api";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import { FileList, type ListNav } from "@/components/FileList";
import type { Item } from "@/components/fileList/layout";
import { StyleKitContext } from "@/components/style";
import { macKit } from "@/components/style/mac";
import { GalleryView, galleryItem } from "@/components/style/mac/gallery";
import { positionOf } from "@/components/style/mac/quickLook";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// The previews every style shares are tested on their own
vi.mock("@/components/FileViewer", () => ({
  FileViewer: ({ node, autoPlay }: { node: { name: string }; autoPlay?: boolean }) => (
    <p data-preview data-autoplay={String(autoPlay)}>
      {node.name}
    </p>
  ),
}));

const node = (name: string): Item => ({
  id: name,
  parent_id: "folder",
  kind: name.includes(".") ? "file" : "folder",
  name,
  size: 2048,
  mime: "",
  created_at: 1_700_000_000,
  updated_at: 1_700_000_000,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "amy",
  is_favorite: false,
});
const ITEMS = ["Drafts", "beach.jpg", "clip.mp4", "report.pdf"].map(node);
const source: FileSource = { contentUrl: () => "", thumbUrl: () => "", downloadLink: () => Promise.resolve("") };

/** A box scrolling across, 1000 pixels wide, which the strip is laid out in (the test page has no layout or style sheet) */
function scrollingBox() {
  const el = document.createElement("div");
  el.style.overflowX = "auto";
  Object.defineProperty(el, "offsetWidth", { value: 1000 });
  Object.defineProperty(el, "offsetHeight", { value: 90 });
  document.body.append(el);
  return el;
}

describe("the item shown", () => {
  test("of the items selected: the one with the focus, else the item clicked last, else the first", () => {
    const [a, b, c] = ITEMS;
    expect(galleryItem([a, b, c], c.id, b.id)).toBe(c);
    expect(galleryItem([a, b, c], "elsewhere", b.id)).toBe(b);
    expect(galleryItem([a, b, c], null, null)).toBe(a);
    expect(galleryItem([], a.id, a.id)).toBeUndefined();
  });

  test("its place in the list: by the list's index in a large folder, else by looking", () => {
    expect(positionOf("clip.mp4", ITEMS)).toBe(2);
    expect(positionOf("clip.mp4", [undefined, undefined, ITEMS[2]])).toBe(2);
    expect(positionOf("far", ITEMS, new Map([["far", 4000]]))).toBe(4000);
    expect(positionOf("gone", ITEMS)).toBe(-1);
  });
});

describe("the view", () => {
  let root: Root | null = null;
  let el: HTMLElement;
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    el.remove();
    localStorage.clear();
    vi.useRealTimers();
  });

  const calls = { open: vi.fn<(n: Item) => void>(), setDialog: vi.fn<(d: unknown) => void>(), download: vi.fn<() => void>() };

  /** The explorer's state as far as the view uses it, with a selection kept as the explorer keeps it */
  function Harness({ items }: { items: Item[] }) {
    const [selected, setSelected] = useState(new Set<string>());
    const [anchor, setAnchor] = useState<string | null>(null);
    const nav = { current: null as ListNav | null };
    const selectedNodes = items.filter((n) => selected.has(n.id));
    const s = {
      kit: macKit,
      shown: items,
      items,
      total: items.length,
      selected,
      selectedNodes,
      anchor,
      count: selectedNodes.length,
      picked: { ids: [...selected], span: null, count: selected.size },
      dialog: null,
      caps: { write: true, del: true, share: true, manage: false },
      setSelected,
      setAnchor,
      arriving: () => false,
      listNav: nav,
      setDialog: calls.setDialog,
      setDetailsOpen: vi.fn<(open: boolean) => void>(),
    } as unknown as ExplorerState;
    const a = { open: calls.open, download: calls.download } as unknown as ExplorerActions;
    const p = { items, crumbs: [{ label: "Holiday" }], loading: false, folderId: "folder" } as unknown as ExplorerProps;
    return (
      <StyleKitContext.Provider value={macKit}>
        <GalleryView
          p={p}
          s={s}
          a={a}
          empty={<p data-empty>Nothing here</p>}
          list={{
            items,
            view: "gallery",
            source,
            selected,
            anchor,
            onSelect: (next, at) => {
              setSelected(next);
              if (at !== undefined) setAnchor(at);
            },
            onOpen: calls.open,
            navRef: nav,
          }}
        />
      </StyleKitContext.Provider>
    );
  }

  function render(items = ITEMS) {
    el = scrollingBox();
    root = createRoot(el);
    act(() => root!.render(<Harness items={items} />));
  }
  const option = (id: string) => el.querySelector<HTMLElement>(`[role="option"][data-node-id="${CSS.escape(id)}"]`)!;
  const selectedIds = () => [...el.querySelectorAll('[role="option"][aria-selected="true"]')].map((o) => o.getAttribute("data-node-id"));
  const region = () => el.querySelector<HTMLElement>("section[aria-label]")!;
  const said = () => [...el.querySelectorAll('[role="status"]')].map((s) => s.textContent).join(" | ");
  const key = (target: HTMLElement, k: string, mods: KeyboardEventInit = {}) =>
    act(() => void target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...mods })));
  const wait = (ms: number) => act(() => void vi.advanceTimersByTime(ms));
  const button = (name: string) => [...el.querySelectorAll("button")].find((b) => b.getAttribute("aria-label") === name || b.textContent!.trim() === name);

  test("starts on the first item: its preview in a region named after it, and the strip, a listbox across named Thumbnails", () => {
    render();
    expect(selectedIds()).toEqual(["Drafts"]);
    expect(region().getAttribute("aria-label")).toBe("Preview: Drafts");
    // A folder has no preview: its card
    expect(region().querySelector("[data-preview]")).toBeNull();
    expect(region().textContent).toContain("1 of 4");
    const strip = el.querySelector<HTMLElement>('[role="listbox"]')!;
    expect(strip.getAttribute("aria-label")).toBe("Thumbnails");
    expect(strip.getAttribute("aria-orientation")).toBe("horizontal");
    expect([...strip.querySelectorAll('[role="option"]')].map((o) => o.getAttribute("data-node-id"))).toEqual(ITEMS.map((n) => n.id));
    // Each thumbnail is named after its item
    expect(option("beach.jpg").textContent).toBe("beach.jpg");
    // Nothing announced yet
    expect(said()).not.toContain("of 4");
  });

  test("→ and ← move through the items; the preview follows once the selection stays, without playing; each move is announced", () => {
    render();
    option("Drafts").focus();
    key(option("Drafts"), "ArrowRight");
    expect(selectedIds()).toEqual(["beach.jpg"]);
    expect(document.activeElement).toBe(option("beach.jpg"));
    expect(region().getAttribute("aria-label")).toBe("Preview: beach.jpg");
    expect(said()).toContain("beach.jpg, 2 of 4");
    // Its thumbnail at first, then the preview
    expect(region().querySelector("[data-preview]")).toBeNull();
    wait(250);
    expect(region().querySelector("[data-preview]")!.textContent).toBe("beach.jpg");
    key(option("beach.jpg"), "ArrowRight");
    wait(250);
    expect(region().querySelector("[data-preview]")!.getAttribute("data-autoplay")).toBe("false");
    expect(said()).toContain("clip.mp4, 3 of 4");
    key(option("clip.mp4"), "ArrowLeft");
    expect(selectedIds()).toEqual(["beach.jpg"]);
    key(option("beach.jpg"), "End");
    expect(selectedIds()).toEqual(["report.pdf"]);
    expect(said()).toContain("report.pdf, 4 of 4");
    // Shift extends the selection; the preview shows the item moved to
    key(option("report.pdf"), "ArrowLeft", { shiftKey: true });
    expect(selectedIds()).toEqual(["clip.mp4", "report.pdf"]);
    expect(region().getAttribute("aria-label")).toBe("Preview: clip.mp4");
  });

  test("the details panel: what the item is and what to do with it, hidden and shown again", () => {
    render();
    act(() => option("report.pdf").click());
    const panel = () => el.querySelector<HTMLElement>('aside[aria-label="Details"]');
    expect(panel()!.textContent).toContain("report.pdf");
    expect(panel()!.textContent).toContain("2.0 KB");
    act(() => button("Open")!.click());
    expect(calls.open).toHaveBeenCalledWith(expect.objectContaining({ id: "report.pdf" }));
    act(() => button("Rename")!.click());
    expect(calls.setDialog).toHaveBeenLastCalledWith({ t: "rename", node: expect.objectContaining({ id: "report.pdf" }) });
    act(() => button("Move to trash")!.click());
    expect(calls.setDialog).toHaveBeenLastCalledWith({ t: "trash", picked: expect.objectContaining({ ids: ["report.pdf"], count: 1 }) });

    const toggle = button("Hide details")!;
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    act(() => toggle.click());
    expect(panel()).toBeNull();
    expect(button("Show details")!.getAttribute("aria-pressed")).toBe("false");
    // Kept in the browser
    expect(localStorage.getItem("tf-gallery-details")).toBe("false");
    act(() => button("Show details")!.click());
    expect(panel()).not.toBeNull();
  });

  test("an empty list shows what an empty list shows", () => {
    render([]);
    expect(el.querySelector("[data-empty]")).not.toBeNull();
    expect(el.querySelector('[role="listbox"]')).toBeNull();
  });
});

describe("the strip in a large folder", () => {
  let root: Root | null = null;
  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.replaceChildren();
  });

  test("holds a place for each item not loaded, and asks for the positions in view; End goes to the last item once its part loads", () => {
    const onShow = vi.fn<(first: number, last: number) => void>();
    const onSelect = vi.fn<(s: Set<string>) => void>();
    const items: (Item | undefined)[] = Array.from({ length: 3000 }, (_, i) => (i < 40 ? node(`Item ${i}.jpg`) : undefined));
    const el = scrollingBox();
    root = createRoot(el);
    const nav = { current: null as ListNav | null };
    const list = (shown: (Item | undefined)[]) => (
      <StyleKitContext.Provider value={macKit}>
        <FileList items={shown} onShow={onShow} view="gallery" source={source} selected={new Set()} anchor={null} onSelect={onSelect} onOpen={() => {}} navRef={nav} label="Thumbnails" />
      </StyleKitContext.Provider>
    );
    act(() => root!.render(list(items)));
    const box = el.querySelector<HTMLElement>('[role="listbox"]')!;
    expect(box.getAttribute("aria-orientation")).toBe("horizontal");
    // Every item has its width: the strip is as long as the folder
    expect(onShow).toHaveBeenCalled();
    expect(onShow.mock.lastCall![0]).toBe(0);
    const first = box.querySelector<HTMLElement>('[role="option"]')!;
    first.focus();
    act(() => void first.dispatchEvent(new KeyboardEvent("keydown", { key: "End", bubbles: true, cancelable: true })));
    // Not loaded yet: nothing selected until its part comes
    expect(onSelect).not.toHaveBeenCalled();
    const last = node("Item 2999.jpg");
    act(() => root!.render(list(items.map((n, i) => (i === 2999 ? last : n)))));
    expect(onSelect).toHaveBeenLastCalledWith(new Set(["Item 2999.jpg"]), "Item 2999.jpg", null);
  });
});
