/**
 * New items, as in File Explorer (the Windows style; `newAtEnd` in components/style): a new folder or text document
 * shows at once at the end of the list, in rename mode, while the server makes it, and stays there after renaming. It
 * only moves into its sorted place after a refresh, a change of sort, or leaving the folder. In a large folder not all
 * loaded (lib/windows) it stays after the items that were loaded when it was made, rather than going to its place in a
 * part that isn't loaded.
 */
import { useEffect, useMemo, useState } from "react";
import { createStore, useStore } from "@/lib/store";
import type { Item } from "./types";

/** A new item kept where it was made */
export interface NewItem {
  /** What the list shows: until the server has made it and it isn't being renamed, an item with a temporary id (`isPending`) */
  item: Item;
  /** Where it shows among the folder's items: after the ones loaded when it was made (Infinity: at the end) */
  at: number;
  /** The id the server gave it; null while it is being made */
  id: string | null;
  /** Resolves to its id once made (the rename typed meanwhile waits for it); rejected when making it failed */
  made: Promise<string>;
  /** The list has had it: once it no longer has it, it was deleted or moved away */
  seen: boolean;
}

const TEMPORARY = "new:";

/** An item shown while the server makes it: nothing can be done with it until then, except naming it */
export function isPending(id: string) {
  return id.startsWith(TEMPORARY);
}

/** Refreshes (F5, the Refresh button or menu item): new items go to their sorted places */
const refreshes = createStore(0);
export function listRefreshed() {
  refreshes.set(refreshes.get() + 1);
}

/**
 * The new items of the folder shown. `place` is the folder and its order: changing either lets them go to their
 * sorted places. A refresh does too, except for one being made, or being renamed (`keep`).
 */
export function useNewItems(place: string, keep: (n: NewItem) => boolean) {
  const [items, setItems] = useState<NewItem[]>([]);
  const refreshed = useStore(refreshes);
  // oxlint-disable-next-line react-hooks/exhaustive-deps -- when the list is refreshed, with what is kept then
  useEffect(() => setItems((all) => all.filter((n) => n.id === null || keep(n))), [refreshed]);
  useEffect(() => setItems([]), [place]);

  return useMemo(() => {
    const update = (key: string, change: (n: NewItem) => NewItem | null) => setItems((all) => all.flatMap((n) => (n.item.id === key ? (change(n) ?? []) : [n])));
    return {
      items,
      /** Shows an item being made, at `at`: `made` resolves to its id. The item shown, with its temporary id */
      add(item: Omit<Item, "id">, at: number, made: Promise<string>) {
        const shown: Item = { ...item, id: `${TEMPORARY}${crypto.randomUUID()}` };
        setItems((all) => [...all, { item: shown, at, id: null, made, seen: false }]);
        return shown;
      },
      /** The server made it */
      made(key: string, id: string) {
        update(key, (n) => ({ ...n, id }));
      },
      /** Shown as the item it is now, with its real id (once it isn't being renamed: the rename box would start again) */
      settle(key: string) {
        update(key, (n) => (n.id ? { ...n, item: { ...n.item, id: n.id } } : n));
      },
      /** Renamed: the name it shows until the list has it */
      rename(key: string, name: string) {
        update(key, (n) => ({ ...n, item: { ...n.item, name } }));
      },
      /** Making it failed, or it was deleted or moved away: it goes */
      drop(keys: Iterable<string>) {
        const gone = new Set(keys);
        setItems((all) => (all.some((n) => gone.has(n.item.id)) ? all.filter((n) => !gone.has(n.item.id)) : all));
      },
      /** The list has these items: the new ones among them are marked as seen */
      see(loaded: ReadonlyMap<string, unknown>) {
        setItems((all) => (all.some((n) => !n.seen && n.id && loaded.has(n.id)) ? all.map((n) => (!n.seen && n.id && loaded.has(n.id) ? { ...n, seen: true } : n)) : all));
      },
    };
  }, [items]);
}

/** The new items that show: one the list had and no longer has was deleted or moved away */
function showing(n: NewItem, loaded: ReadonlyMap<string, Item>) {
  return !n.seen || !n.id || loaded.has(n.id);
}

/**
 * The items as the list shows them: the folder's items (`base`, by position; a large folder has gaps where parts aren't
 * loaded), with the new ones taken out of their sorted places and put where they were made. `loaded` has the latest
 * copy of each item, from the server.
 */
export function arrange(base: readonly (Item | undefined)[], added: readonly NewItem[], loaded: ReadonlyMap<string, Item>): readonly (Item | undefined)[] {
  if (!added.length) return base;
  const kept = new Set(added.flatMap((n) => (n.id ? [n.item.id, n.id] : [n.item.id])));
  const out = base.filter((x) => !x || !kept.has(x.id));
  let k = 0;
  for (const n of added) {
    if (!showing(n, loaded)) continue;
    // The server's copy once the item shown has taken its id and the list has it
    const shown = (n.item.id === n.id && loaded.get(n.id)) || n.item;
    out.splice(Math.min(n.at + k++, out.length), 0, shown);
  }
  return out;
}

/** The new items the list hasn't loaded (sorted into a part not loaded, or not loaded yet), as they show */
export function notLoaded(added: readonly NewItem[], loaded: ReadonlyMap<string, Item>) {
  return added.filter((n) => n.id && n.item.id === n.id && !loaded.has(n.id) && showing(n, loaded)).map((n) => n.item);
}
