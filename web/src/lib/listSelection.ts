import { useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import { findByPrefix } from "@/lib/keys";

/**
 * Selecting in a list with the mouse and the keyboard, the way the file list does it (like File Explorer), for the
 * smaller lists: spaces, the control panel, storage locations and the admin tables.
 *
 * - Click selects; with `multi`, Ctrl toggles and Shift selects a range.
 * - One item is a Tab stop (the first selected one, else the first); the arrows move the selection, Home and End go to
 *   the ends, and in a grid of tiles Left and Right go to the item before and after. With `multi`, Shift extends the
 *   selection, Ctrl moves only the focus, Ctrl+Space adds or removes the focused item, Ctrl+A selects everything and
 *   Esc selects nothing. Space selects the focused item; Enter (or a double click) opens it, a middle click opens it
 *   in a new tab. Typing the start of a name goes to it (with `nameOf`).
 *
 * The items are found in the page, in the element given `scopeProps` (or the list itself): each run of items with
 * the same parent is a row, or a grid whose columns are read from its CSS. Put `listProps` on each element holding
 * items (with the role of a listbox or grid), and `itemProps` on each item (with the role of an option or row).
 */
export function useSelectableList<T>(o: {
  /** In the order shown */
  items: readonly T[];
  keyOf(item: T): string;
  selected: ReadonlySet<string>;
  onSelect(next: Set<string>): void;
  multi?: boolean;
  onOpen?(item: T, newTab: boolean): void;
  nameOf?(item: T): string;
  /** Items that can't take the focus now (in a collapsed section, say): the Tab stop isn't put on them */
  hidden?(item: T): boolean;
}) {
  const [anchor, setAnchor] = useState<string | null>(null);
  const typed = useRef({ text: "", at: 0 });
  const keys = o.items.map(o.keyOf);
  const shown = o.items.filter((x) => !o.hidden?.(x));
  const stop = shown.find((x) => o.selected.has(o.keyOf(x))) ?? shown[0];
  const stopKey = stop === undefined ? null : o.keyOf(stop);

  const only = (key: string | null) => {
    o.onSelect(new Set(key === null ? [] : [key]));
    setAnchor(key);
  };
  const toggle = (key: string) => {
    const next = new Set(o.selected);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    o.onSelect(next);
    setAnchor(key);
  };
  /** From the anchor to `key` (with Ctrl, added to what is selected) */
  const range = (key: string, add: boolean) => {
    const a = anchor === null ? -1 : keys.indexOf(anchor);
    const b = keys.indexOf(key);
    if (a < 0 || b < 0) return only(key);
    const next = new Set(add ? o.selected : []);
    for (let i = Math.min(a, b); i <= Math.max(a, b); i++) next.add(keys[i]);
    o.onSelect(next);
  };

  const click = (e: MouseEvent, item: T) => {
    const key = o.keyOf(item);
    // Lists that start a marquee on mousedown keep items from taking the focus: arrows go on from the clicked one
    (e.currentTarget as HTMLElement).focus({ preventScroll: true });
    if (o.multi && e.shiftKey) range(key, e.ctrlKey || e.metaKey);
    else if (o.multi && (e.ctrlKey || e.metaKey)) toggle(key);
    else only(key);
  };

  const keyDown = (e: KeyboardEvent<HTMLElement>, item: T) => {
    // Keys typed in a control inside the item (a rename box, a button) belong to it
    if (e.target !== e.currentTarget || e.altKey) return;
    const key = o.keyOf(item);
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "Enter") {
      e.preventDefault();
      if (!e.repeat) o.onOpen?.(item, mod);
      return;
    }
    if (e.key === " ") {
      e.preventDefault();
      if (o.multi && mod) toggle(key);
      else only(key);
      return;
    }
    if (o.multi && mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      o.onSelect(new Set(keys));
      return;
    }
    if (o.multi && e.key === "Escape" && o.selected.size > 0) {
      e.preventDefault();
      o.onSelect(new Set());
      return;
    }
    const el = e.currentTarget;
    const target = e.key.length === 1 && !mod ? typeAhead(e.key, item) : move(el, e.key);
    if (target === undefined) return;
    e.preventDefault();
    if (target === null) return;
    const next = target.dataset.itemKey!;
    if (o.multi && e.shiftKey) range(next, mod);
    else if (!(o.multi && mod)) only(next);
    target.focus();
    target.scrollIntoView({ block: "nearest" });
  };

  /** The item to go to for an arrow, Home or End (null: none there), or undefined for other keys */
  const move = (el: HTMLElement, key: string): HTMLElement | null | undefined => {
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(key)) return undefined;
    const scope = el.closest("[data-item-scope]") ?? el.closest("[data-item-list]");
    const all = Array.from(scope?.querySelectorAll<HTMLElement>("[data-item-key]") ?? [el]);
    // Runs of items with the same parent: a table's rows, or a section's grid of tiles
    const groups: HTMLElement[][] = [];
    for (const x of all) {
      if (groups.at(-1)?.[0].parentElement === x.parentElement) groups.at(-1)!.push(x);
      else groups.push([x]);
    }
    const g = groups.findIndex((group) => group.includes(el));
    const group = groups[g];
    const i = group.indexOf(el);
    const perRow = columnsOf(el.parentElement);
    const col = i % perRow;
    if (key === "Home") return all[0] ?? null;
    if (key === "End") return all.at(-1) ?? null;
    if (key === "ArrowRight" || key === "ArrowLeft") return perRow > 1 ? (all[all.indexOf(el) + (key === "ArrowRight" ? 1 : -1)] ?? null) : undefined;
    if (key === "ArrowDown") {
      if (i + perRow < group.length) return group[i + perRow];
      // Below is empty but there is a shorter last row: its last item
      if (Math.floor(i / perRow) < Math.floor((group.length - 1) / perRow)) return group.at(-1)!;
      const below = groups[g + 1];
      return below ? below[Math.min(col, below.length - 1)] : null;
    }
    if (i - perRow >= 0) return group[i - perRow];
    const above = groups[g - 1];
    if (!above) return null;
    const cols = columnsOf(above[0].parentElement);
    return above[Math.min(Math.floor((above.length - 1) / cols) * cols + Math.min(col, cols - 1), above.length - 1)];
  };

  /** Typing the start of a name: the next item whose name starts with what was typed in the last second */
  const typeAhead = (char: string, item: T): HTMLElement | null | undefined => {
    if (!o.nameOf || char === " ") return undefined;
    const now = Date.now();
    const t = typed.current;
    t.text = now - t.at < 1000 ? t.text + char : char;
    t.at = now;
    const from = o.items.indexOf(item);
    // Typing the same letter again goes on to the next item with it
    const again = t.text.length > 1 && [...t.text].every((c) => c === t.text[0]);
    const found = findByPrefix(o.items.map(o.nameOf), again ? char : t.text, again || t.text.length === 1 ? from : from - 1);
    if (found < 0) return null;
    const key = o.keyOf(o.items[found]);
    return document.querySelector<HTMLElement>(`[data-item-key="${CSS.escape(key)}"]`);
  };

  return {
    anchor,
    /** Select only this item (null: nothing), as the start of a range */
    selectOnly: only,
    /** On the element holding all the items the arrows move between (several lists, say) */
    scopeProps: { "data-item-scope": true } as const,
    /** On each element holding items: `role` is "listbox" for tiles, "grid" for a table */
    listProps: (role: "listbox" | "grid", label: string) => ({
      role,
      "aria-label": label,
      "aria-multiselectable": o.multi || undefined,
      "data-item-list": true,
    }),
    itemProps: (item: T) => {
      const key = o.keyOf(item);
      return {
        "data-item-key": key,
        "aria-selected": o.selected.has(key),
        tabIndex: key === stopKey ? 0 : -1,
        onKeyDown: (e: KeyboardEvent<HTMLElement>) => keyDown(e, item),
        onClick: (e: MouseEvent) => {
          e.stopPropagation();
          click(e, item);
        },
        onDoubleClick: () => o.onOpen?.(item, false),
        onAuxClick: (e: MouseEvent) => {
          if (e.button !== 1 || !o.onOpen) return;
          e.preventDefault();
          o.onOpen(item, true);
        },
        // A middle click doesn't start scrolling
        onMouseDown: (e: MouseEvent) => e.button === 1 && e.preventDefault(),
        // Right-clicking an item that isn't selected selects only it
        onContextMenu: () => !o.selected.has(key) && only(key),
      };
    },
  };
}

/** Items to a row in a CSS grid (1 for anything else) */
function columnsOf(el: HTMLElement | null) {
  if (!el) return 1;
  const style = getComputedStyle(el);
  if (style.display !== "grid") return 1;
  return Math.max(1, style.gridTemplateColumns.split(" ").filter(Boolean).length);
}
