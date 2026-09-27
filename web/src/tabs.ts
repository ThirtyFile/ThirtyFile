//! Browser-style tabs. Each tab has its own history (back/forward only move within the tab);
//! switching tabs navigates with replace, so the browser history doesn't get mixed together.

import { useCallback, useSyncExternalStore } from "react";
import { useNavigate } from "react-router";
import { confirm } from "@/components/confirm";
import { hasDraft, setDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";

export interface Tab {
  id: string;
  /** History within a tab (pathname + search) */
  entries: string[];
  index: number;
  title: string;
}

interface TabsState {
  tabs: Tab[];
  active: string;
}

const HOME = "/files";
const MAX_TABS = 20;
const MAX_ENTRIES = 50;

let seq = 0;
let storageKey = "tf-tabs";
let state: TabsState = fresh(HOME);
/** The next navigation, triggered by a tab operation, that shouldn't be written to the history */
let pending: string | null = null;
const listeners = new Set<() => void>();

function newTab(path: string): Tab {
  return { id: `t${Date.now().toString(36)}${seq++}`, entries: [path], index: 0, title: "" };
}

function fresh(path: string): TabsState {
  const t = newTab(path);
  return { tabs: [t], active: t.id };
}

function set(next: TabsState) {
  state = next;
  try {
    localStorage.setItem(storageKey, JSON.stringify(state));
  } catch {
    // Ignore when storage isn't available
  }
  listeners.forEach((l) => l());
}

function updateTab(id: string, patch: Partial<Tab>) {
  set({ ...state, tabs: state.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) });
}

export function activeTab(): Tab {
  return state.tabs.find((t) => t.id === state.active) ?? state.tabs[0];
}

export function currentEntry(t: Tab) {
  return t.entries[t.index] ?? HOME;
}

/** If the tab is viewing a file (/view/:id), return the file id */
export function viewedFile(t: Tab): string | null {
  const m = /^\/view\/([^/?]+)/.exec(currentEntry(t));
  return m ? m[1] : null;
}

/** Confirm before closing: ask when there are unsaved edits, and only discard them once closing is confirmed */
async function confirmClose(tab: Tab): Promise<boolean> {
  const file = viewedFile(tab);
  if (!file || !hasDraft(file)) return true;
  const ok = await confirm({
    title: t("Discard unsaved changes?"),
    description: t("\"{name}\" has unsaved changes. If you close the tab, your changes will be lost.", { name: tab.title || t("File") }),
    confirmText: t("Discard and close"),
    destructive: true,
  });
  if (ok) setDraft(file, null);
  return ok;
}

/** Load the user's tabs after signing in */
export function loadTabs(userId: number) {
  const key = `tf-tabs-${userId}`;
  if (key === storageKey) return;
  storageKey = key;
  try {
    const raw = localStorage.getItem(key);
    const saved = raw ? validTabs(JSON.parse(raw)) : null;
    state = saved ?? fresh(HOME);
  } catch {
    state = fresh(HOME);
  }
  listeners.forEach((l) => l());
}

/** The saved state, if it has the expected shape (an older format or an edited value would otherwise crash the tab bar on every load) */
export function validTabs(raw: unknown): TabsState | null {
  if (!raw || typeof raw !== "object") return null;
  const { tabs, active } = raw as { tabs?: unknown; active?: unknown };
  if (!Array.isArray(tabs) || tabs.length === 0 || tabs.length > MAX_TABS || typeof active !== "string") return null;
  const ok = (t: unknown): t is Tab => {
    if (!t || typeof t !== "object") return false;
    const { id, entries, index, title } = t as Record<string, unknown>;
    return (
      typeof id === "string" &&
      Array.isArray(entries) &&
      entries.length > 0 &&
      entries.every((e) => typeof e === "string" && e.startsWith("/")) &&
      Number.isInteger(index) &&
      (index as number) >= 0 &&
      (index as number) < entries.length &&
      typeof title === "string"
    );
  };
  if (!tabs.every(ok)) return null;
  const ids = new Set(tabs.map((t) => t.id));
  if (ids.size !== tabs.length || !ids.has(active)) return null;
  return { tabs, active };
}

export function useTabsState() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    () => state,
  );
}

/** Called on every URL change: record it in the current tab's history */
export function syncLocation(loc: string) {
  if (pending !== null) {
    const expected = pending;
    pending = null;
    if (expected === loc) return;
  }
  const tab = activeTab();
  if (currentEntry(tab) === loc) return;
  const entries = [...tab.entries.slice(0, tab.index + 1), loc].slice(-MAX_ENTRIES);
  updateTab(tab.id, { entries, index: entries.length - 1 });
}

/** Title of the current page (provided by Frame) */
export function setActiveTitle(title: string) {
  const tab = activeTab();
  if (tab.title !== title) updateTab(tab.id, { title });
}

export function useTabActions() {
  const navigate = useNavigate();
  const go = useCallback(
    (path: string) => {
      pending = path;
      navigate(path, { replace: true });
    },
    [navigate],
  );

  const open = useCallback(
    (path: string = HOME, opts?: { reuse?: boolean }) => {
      // When the file is already open in some tab, switch to it directly
      if (opts?.reuse) {
        const existing = state.tabs.find((t) => currentEntry(t) === path);
        if (existing) {
          if (existing.id !== state.active) {
            set({ ...state, active: existing.id });
            go(path);
          }
          return;
        }
      }
      if (state.tabs.length >= MAX_TABS) return;
      const t = newTab(path);
      const i = state.tabs.findIndex((x) => x.id === state.active);
      const tabs = [...state.tabs];
      tabs.splice(i + 1, 0, t);
      set({ tabs, active: t.id });
      go(path);
    },
    [go],
  );

  /**
   * Open a file: navigate to it in the current tab ("Back" returns to the original folder);
   * if the file is already open in another tab, switch there, so the same file never has two editing views at once
   */
  const openFile = useCallback(
    (path: string) => {
      const existing = state.tabs.find((t) => t.id !== state.active && currentEntry(t) === path);
      if (existing) {
        set({ ...state, active: existing.id });
        go(path);
      } else {
        navigate(path);
      }
    },
    [go, navigate],
  );

  const activate = useCallback(
    (id: string) => {
      const t = state.tabs.find((x) => x.id === id);
      if (!t || id === state.active) return;
      set({ ...state, active: id });
      go(currentEntry(t));
    },
    [go],
  );

  const close = useCallback(
    async (id: string) => {
      const tab = state.tabs.find((x) => x.id === id);
      if (!tab || !(await confirmClose(tab))) return;
      // Tabs may have changed while the question was open
      const i = state.tabs.findIndex((x) => x.id === id);
      if (i < 0) return;
      if (state.tabs.length === 1) {
        // Last tab: go back to the home page instead of closing
        const t = newTab(HOME);
        set({ tabs: [t], active: t.id });
        go(HOME);
        return;
      }
      const tabs = state.tabs.filter((x) => x.id !== id);
      if (id !== state.active) {
        set({ ...state, tabs });
        return;
      }
      const next = tabs[Math.min(i, tabs.length - 1)];
      set({ tabs, active: next.id });
      go(currentEntry(next));
    },
    [go],
  );

  const closeOthers = useCallback(async (id: string) => {
    if (!state.tabs.some((x) => x.id === id)) return;
    // Ask once for all of them, so cancelling can't leave some drafts already discarded
    const unsaved = state.tabs.filter((x) => x.id !== id).map(viewedFile).filter((f): f is string => !!f && hasDraft(f));
    if (
      unsaved.length &&
      !(await confirm({
        title: t("Discard unsaved changes?"),
        description: t("{n} tab has unsaved changes. If you close it, the changes will be lost.|{n} tabs have unsaved changes. If you close them, the changes will be lost.", { n: unsaved.length }),
        confirmText: t("Discard and close"),
        destructive: true,
      }))
    )
      return;
    // Tabs may have changed while the question was open
    const tab = state.tabs.find((x) => x.id === id);
    if (!tab) return;
    unsaved.forEach((f) => setDraft(f, null));
    set({ tabs: [tab], active: tab.id });
    if (id !== state.active) go(currentEntry(tab));
  }, [go]);

  const step = useCallback(
    (delta: number) => {
      const t = activeTab();
      const index = t.index + delta;
      if (index < 0 || index >= t.entries.length) return;
      updateTab(t.id, { index });
      go(t.entries[index]);
    },
    [go],
  );

  const move = useCallback((from: string, to: string) => {
    const tabs = [...state.tabs];
    const a = tabs.findIndex((t) => t.id === from);
    const b = tabs.findIndex((t) => t.id === to);
    if (a < 0 || b < 0 || a === b) return;
    const [t] = tabs.splice(a, 1);
    tabs.splice(b, 0, t);
    set({ ...state, tabs });
  }, []);

  return { open, openFile, activate, close, closeOthers, back: () => step(-1), forward: () => step(1), move };
}
