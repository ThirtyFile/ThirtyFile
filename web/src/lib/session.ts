import { createContext, useContext } from "react";
import type { Me } from "@/api";
import { createStore, useStore, type Store } from "@/lib/store";

export const MeContext = createContext<Me | null>(null);

export function useMe(): Me {
  const me = useContext(MeContext);
  if (!me) throw new Error("useMe must be used after sign-in");
  return me;
}

/** Whether a stored value has the shape of the default: the same type, and for objects the same keys with the same types */
export function sameShape(value: unknown, like: unknown): boolean {
  if (Array.isArray(like)) return Array.isArray(value);
  if (like === null || typeof like !== "object") return typeof value === typeof like && (typeof value !== "number" || Number.isFinite(value));
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  return Object.entries(like).every(([k, v]) => sameShape((value as Record<string, unknown>)[k], v));
}

/** One store per key, so every component showing a preference sees a change at once, also one made in another window */
const stores = new Map<string, Store<unknown>>();

function storeOf(key: string, initial: unknown, valid: (v: never) => boolean) {
  let store = stores.get(key);
  if (!store) {
    store = createStore(read(key, initial, valid));
    stores.set(key, store);
  }
  return store;
}

/** The stored value, or `initial` when there is none or it doesn't have the expected shape (an older format, an edit by hand) */
function read(key: string, initial: unknown, valid: (v: never) => boolean): unknown {
  try {
    const raw = localStorage.getItem(key);
    const value: unknown = raw === null ? initial : JSON.parse(raw);
    return sameShape(value, initial) && valid(value as never) ? value : initial;
  } catch {
    return initial;
  }
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    const store = e.key && stores.get(e.key);
    if (!store) return;
    // The default is only known to the hooks: compare with the value this window had
    try {
      const value: unknown = e.newValue === null ? undefined : JSON.parse(e.newValue);
      if (value === undefined || !sameShape(value, store.get())) return;
      store.set(value);
    } catch {
      // Not ours to read
    }
  });
}

/** Preferences stored in localStorage (view mode, sort, pane widths); `valid` checks more than the shape */
export function usePersisted<T>(key: string, initial: T, valid: (v: T) => boolean = () => true): [T, (v: T) => void] {
  const store = storeOf(key, initial, valid);
  const value = useStore(store) as T;
  const set = (v: T) => {
    try {
      localStorage.setItem(key, JSON.stringify(v));
    } catch {
      // Storage blocked by the browser: the value lasts until the page is closed
    }
    store.set(v);
  };
  return [value, set];
}
