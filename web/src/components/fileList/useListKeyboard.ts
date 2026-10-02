//! Moving through the file list with the keyboard: arrows, Home and End, PageUp and PageDown (Shift extends the
//! selection, Ctrl moves only the focus), Space, Enter, the context menu key, and typing to find an item

import { useLayoutEffect, useRef, type KeyboardEvent, type RefObject } from "react";
import type { Virtualizer } from "@tanstack/react-virtual";
import type { FileListProps } from "@/components/FileList";
import { HEAD, PAD, ROW, type Item, type Layout, type TILED } from "@/components/fileList/layout";
import { pageRows } from "@/lib/listView";
import { findByPrefix } from "@/lib/keys";
import type { ListSpan } from "@/lib/span";

/** What the list knows that its keyboard handling works with */
export interface ListKeyboard {
  p: FileListProps;
  /** In the order shown (group by group); items not loaded yet are empty */
  items: readonly (Item | undefined)[];
  n: number;
  indexOf: Map<string, number>;
  span: ListSpan | null;
  layout: Layout;
  rowOf(index: number): number;
  v: Virtualizer<HTMLElement, Element>;
  root: RefObject<HTMLElement | null>;
  grid: boolean;
  tile: (typeof TILED)[keyof typeof TILED] | null;
  scroller: HTMLElement | null;
  focusId: string | null;
  setFocusId(id: string | null): void;
  /** The item Tab reaches */
  tabStop: number;
}

/**
 * The list's keyboard handling: `keyNav` for a row's keys, `toggle` and `rangeTo` for clicks with Ctrl and Shift too,
 * `focusItem`, and `keyMenu` (when a context menu was just opened from the keyboard). Also answers the explorer's
 * `navRef` (typing to find an item, showing one).
 */
export function useListKeyboard({ p, items, n, indexOf, span, layout, rowOf, v, root, grid, tile, scroller, focusId, setFocusId, tabStop }: ListKeyboard) {
  const pendingFocus = useRef<string | null>(null);

  // Keyboard moves: focus the item once its row is rendered; a move to an item not loaded yet is made once it loads
  useLayoutEffect(() => {
    const nav = pendingNav.current;
    if (nav && items[nav.index]) {
      pendingNav.current = null;
      moveTo(nav.index, nav.mode);
    }
    const id = pendingFocus.current;
    const el = id && root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(id)}"]`);
    if (el) {
      pendingFocus.current = null;
      el.focus({ preventScroll: true });
    }
  });

  const focusItem = (index: number) => {
    const item = items[index];
    v.scrollToIndex(rowOf(index));
    if (!item) return;
    setFocusId(item.id);
    const el = root.current?.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(item.id)}"]`);
    if (el) el.focus({ preventScroll: true });
    else pendingFocus.current = item.id;
  };

  /**
   * The selection from the anchor to an item (Shift), with the items in `keep` (Ctrl+Shift). With items between them
   * not loaded, it is a span from one to the other (the server knows what is between them).
   */
  const rangeTo = (index: number, keep?: Set<string>): { selected: Set<string>; span: ListSpan | null } | null => {
    const anchorIndex = p.anchor === null ? -1 : (indexOf.get(p.anchor) ?? -1);
    if (anchorIndex < 0 || !items[index]) return null;
    const [lo, hi] = [Math.min(anchorIndex, index), Math.max(anchorIndex, index)];
    const next = new Set(keep);
    for (let i = lo; i <= hi; i++) {
      const item = items[i];
      if (!item) {
        // The span holds every item from one end to the other: those kept that it covers (the anchor, at least) would count twice
        const outside = [...(keep ?? [])].filter((id) => {
          const at = indexOf.get(id);
          return at === undefined || at < lo || at > hi;
        });
        return { selected: new Set(outside), span: { from: { id: items[lo]!.id, index: lo }, to: { id: items[hi]!.id, index: hi }, except: new Set() } };
      }
      next.add(item.id);
    }
    return { selected: next, span: null };
  };

  /** Ctrl+click or Ctrl+Space: the item in or out of the selection (in a span, it is left out of it, or back in) */
  const toggle = (index: number) => {
    const item = items[index];
    if (!item) return;
    const id = item.id;
    if (span && index >= (span.from?.index ?? 0) && index <= (span.to?.index ?? Infinity) && !p.selected.has(id)) {
      const except = new Set(span.except);
      if (except.has(id)) except.delete(id);
      else except.add(id);
      p.onSelect(p.selected, id, { ...span, except });
      return;
    }
    const next = new Set(p.selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    p.onSelect(next, id, span);
  };

  /** A keyboard move to an item: select it (or up to it, or only focus it); one not loaded yet is scrolled to, and waited for */
  const pendingNav = useRef<{ index: number; mode: "only" | "range" | "focus" } | null>(null);
  const moveTo = (index: number, mode: "only" | "range" | "focus") => {
    const item = items[index];
    if (!item) {
      pendingNav.current = { index, mode };
      v.scrollToIndex(rowOf(index));
      return;
    }
    if (mode !== "focus") {
      const range = mode === "range" ? rangeTo(index) : null;
      if (range) p.onSelect(range.selected, p.anchor!, range.span);
      else p.onSelect(new Set([item.id]), item.id, null);
    }
    focusItem(index);
  };

  /** The item below or above: in the same column of the next row of items (group headings are skipped), else the last or first item */
  const vertical = (index: number, dir: 1 | -1) => {
    const at = (r: number) => (r >= 0 && r < layout.count ? layout.row(r) : undefined);
    let r = rowOf(index) + dir;
    while (at(r)?.group) r += dir;
    const to = at(r);
    if (!to) return dir > 0 ? n - 1 : 0;
    return Math.min(to.start + index - layout.row(rowOf(index)).start, to.end - 1);
  };

  /** The item a page further down or up (PageDown, PageUp): as many rows as fit in view, less one */
  const page = (index: number, dir: 1 | -1) => {
    const view = scroller === document.documentElement || !scroller ? window.innerHeight : scroller.clientHeight;
    const rowHeight = tile ? tile.h + tile.gap : ROW;
    let at = index;
    for (let k = pageRows(view - (grid ? PAD : HEAD), rowHeight); k > 0; k--) {
      const next = vertical(at, dir);
      if (next === at) break;
      at = next;
    }
    return at;
  };

  /** Shift+F10 or the Menu key: open the context menu at the item, as right-clicking it does (browsers don't all do it for a focused row) */
  const keyMenu = useRef(0);
  const openMenu = (el: HTMLElement) => {
    const r = (el.querySelector("[data-drag-handle]") ?? el).getBoundingClientRect();
    keyMenu.current = Date.now() + 500;
    el.dispatchEvent(new window.MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: r.left + Math.min(r.width / 2, 100), clientY: r.top + r.height / 2 }));
    // A menu opened like a right-click leaves the focus on the row: move it into the menu, so the arrow keys go through its items
    setTimeout(() => {
      const menu = document.querySelector<HTMLElement>("[role=menu][data-open]");
      if (!menu) return;
      menu.focus();
      // Closed without doing anything that takes the focus (a dialog, the rename box): it goes back to the row, not the list around it
      const back = new MutationObserver(() => {
        if (menu.isConnected) return;
        back.disconnect();
        const at = document.activeElement;
        if (el.isConnected && (!at || at === document.body || !at.closest("[role=menu], [role=dialog], input, textarea"))) el.focus({ preventScroll: true });
      });
      back.observe(document.body, { childList: true, subtree: true });
    }, 30);
  };

  /**
   * Keyboard: arrows move the selection (Shift extends it, Ctrl moves only the focus), Space selects (toggles with Ctrl),
   * Home/End and PageUp/PageDown jump, Enter opens, Shift+F10 or the Menu key opens the context menu
   */
  const keyNav = (e: KeyboardEvent<HTMLElement>, index: number) => {
    // Keys typed in a control inside the row (its checkbox, the rename box) belong to that control
    const current = items[index];
    if (e.target !== e.currentTarget || !current || current.id === p.renamingId) return;
    if (e.key === "Enter" && !e.altKey && !e.repeat) {
      e.preventDefault();
      p.onOpen(current, true);
      return;
    }
    if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey)) {
      e.preventDefault();
      openMenu(e.currentTarget);
      return;
    }
    // Alt+arrows move around folders (handled by the address bar)
    if (e.altKey) return;
    let next: number | null = null;
    if (e.key === "ArrowDown") next = vertical(index, 1);
    else if (e.key === "ArrowUp") next = vertical(index, -1);
    else if (e.key === "ArrowRight" && grid) next = Math.min(n - 1, index + 1);
    else if (e.key === "ArrowLeft" && grid) next = Math.max(0, index - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === "PageDown") next = page(index, 1);
    else if (e.key === "PageUp") next = page(index, -1);
    else if (e.key === " ") {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) toggle(index);
      else p.onSelect(new Set([current.id]), current.id, null);
      return;
    }
    if (next === null) return;
    e.preventDefault();
    // Ctrl moves the focus and leaves the selection alone, like File Explorer: Ctrl+Space then adds or removes the item.
    // An item not loaded yet (End in a large folder) is selected once its part has loaded.
    moveTo(next, (e.ctrlKey || e.metaKey) && !e.shiftKey ? "focus" : e.shiftKey ? "range" : "only");
  };

  /** Letters typed to find an item, and when the last one was typed */
  const typed = useRef({ text: "", at: 0 });
  const typeAhead = (key: string) => {
    const now = Date.now();
    const text = (now - typed.current.at < 1000 ? typed.current.text : "") + key.toLocaleLowerCase();
    typed.current = { text, at: now };
    const current = focusId ?? p.anchor;
    const at = current === null ? -1 : (indexOf.get(current) ?? -1);
    // The same letter again moves on to the next item starting with it; more letters narrow down from the current item
    const same = [...text].every((c) => c === text[0]);
    // In a large folder, among the items loaded
    const next = findByPrefix(
      Array.from({ length: n }, (_, i) => items[i]?.name ?? ""),
      same ? text[0] : text,
      same ? at : Math.max(at, 0) - 1,
    );
    if (next < 0) return;
    moveTo(next, "only");
  };
  const show = (id: string, focus: boolean) => {
    const index = indexOf.get(id);
    if (index === undefined) return;
    if (focus) focusItem(index);
    else v.scrollToIndex(rowOf(index));
  };
  const focusStart = () => {
    if (n === 0) return false;
    moveTo(tabStop, "focus");
    return true;
  };
  const scrollTo = (index: number) => index >= 0 && index < n && v.scrollToIndex(rowOf(index), { align: "center" });
  if (p.navRef) p.navRef.current = { typeAhead, show, focusStart, scrollTo };

  return { focusItem, rangeTo, toggle, keyNav, keyMenu };
}
