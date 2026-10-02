//! Click the name of the selected item to rename it, as in File Explorer: a single click on the name of the item that
//! was already the only one selected starts renaming it in place, once the time for a double-click has gone by without
//! a second click. The timing is kept here, apart from the list and the folder tree that use it.

import { useEffect, useRef, useState, type RefObject } from "react";

/** How long a click waits for a second one before it starts renaming (a double-click opens instead) */
export const RENAME_DELAY = 500;

/** A press of a pointer on an item, as the list or the tree sees it */
export interface NamePress {
  /** The item pressed */
  id: string;
  /** On the name text (not its icon or the rest of the row) */
  onName: boolean;
  /** The item was the only one selected before this press: the press that selects it doesn't rename */
  selectedAlone: boolean;
  /** The list (or tree) had the focus before this press: a press that brings the focus from elsewhere doesn't rename */
  hadFocus: boolean;
  /** The person may rename the item */
  canRename: boolean;
  /** Which button: only the main one (a right click cancels) */
  button: number;
  /** The click count: 2 or more is a double-click */
  detail: number;
  /** Ctrl, Shift, Alt or ⌘ held */
  modified: boolean;
  /** "mouse", "pen" or "touch": not on touch input, where a long press selects */
  pointerType: string;
}

/** Whether a press may lead to renaming: the first part of the rules (the click and the wait are the rest) */
export function mayRename(p: NamePress) {
  return p.onName && p.selectedAlone && p.hadFocus && p.canRename && p.button === 0 && p.detail <= 1 && !p.modified && p.pointerType !== "touch";
}

export interface ClickToRename {
  /** A press anywhere (null when not on an item): it cancels a rename waiting, and arms one when `mayRename` */
  press(p: NamePress | null): void;
  /** The click that ends a press on an item: the rename starts after the delay, unless something else happens first */
  click(id: string): void;
  /** Anything else that cancels: a double-click, a right click, dragging, a key, the list changing */
  cancel(): void;
  /** A rename is waiting for the delay to pass */
  readonly waiting: boolean;
}

/** The timing of click-to-rename: `start` is called with the item to rename */
export function clickToRename(start: (id: string) => void, delay = RENAME_DELAY): ClickToRename {
  let armed: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const cancel = () => {
    armed = null;
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  return {
    press(p) {
      cancel();
      if (p && mayRename(p)) armed = p.id;
    },
    click(id) {
      const ready = armed === id;
      cancel();
      if (!ready) return;
      timer = setTimeout(() => {
        timer = null;
        start(id);
      }, delay);
    },
    cancel,
    get waiting() {
      return timer !== null;
    },
  };
}

/** What a list or tree tells `useClickToRename` about its items */
export interface RenameTarget {
  /** Turned off: not the Windows style, or nothing here can be renamed */
  enabled: boolean;
  /** The list or tree: presses outside it cancel, and it must have had the focus */
  root: RefObject<HTMLElement | null>;
  /** The item an element is in (its id), or null */
  itemOf(el: HTMLElement): string | null;
  /** The item is the only one selected */
  selectedAlone(id: string): boolean;
  /** The person may rename the item */
  canRename(id: string): boolean;
  /** Start renaming the item in place */
  start(id: string): void;
}

/**
 * Click-to-rename for a list or tree. It watches the presses and clicks on the whole page: a press anywhere else, a
 * key, a right click, dragging or a double-click cancels a rename waiting. The name text is marked with `data-name`.
 */
export function useClickToRename(target: RenameTarget) {
  const latest = useRef(target);
  latest.current = target;
  const [timing] = useState(() =>
    clickToRename((id) => {
      // Still the one selected, and still allowed, once the wait is over
      const t = latest.current;
      if (t.enabled && t.selectedAlone(id) && t.canRename(id)) t.start(id);
    }),
  );
  useEffect(() => {
    let pointerType = "mouse";
    const inItem = (e: Event) => {
      const t = latest.current;
      const el = e.target as HTMLElement | null;
      return el instanceof HTMLElement && t.root.current?.contains(el) ? { el, id: t.itemOf(el) } : null;
    };
    // Mouse events that follow a touch keep the touch's pointer type
    const onPointer = (e: PointerEvent) => void (pointerType = e.pointerType);
    const onPress = (e: MouseEvent) => {
      const t = latest.current;
      const at = inItem(e);
      if (!t.enabled || !at?.id) return timing.press(null);
      timing.press({
        id: at.id,
        onName: !!at.el.closest("[data-name]"),
        selectedAlone: t.selectedAlone(at.id),
        hadFocus: !!t.root.current?.contains(document.activeElement),
        canRename: t.canRename(at.id),
        button: e.button,
        detail: e.detail,
        modified: e.ctrlKey || e.shiftKey || e.altKey || e.metaKey,
        pointerType,
      });
    };
    const onClick = (e: MouseEvent) => {
      const id = inItem(e)?.id;
      if (id) timing.click(id);
      else timing.cancel();
    };
    const cancel = () => timing.cancel();
    const events: [string, EventListener][] = [
      ["pointerdown", onPointer as EventListener],
      ["mousedown", onPress as EventListener],
      ["click", onClick as EventListener],
      ["dblclick", cancel],
      ["contextmenu", cancel],
      ["dragstart", cancel],
      ["keydown", cancel],
    ];
    for (const [type, fn] of events) document.addEventListener(type, fn, true);
    window.addEventListener("blur", cancel);
    return () => {
      for (const [type, fn] of events) document.removeEventListener(type, fn, true);
      window.removeEventListener("blur", cancel);
      timing.cancel();
    };
  }, [timing]);
  return timing;
}
