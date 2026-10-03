/** How the file list is laid out: its columns (which are shown, and how wide) and how items are grouped */
import { createStore, useStore } from "@/lib/store";
import { t } from "@/lib/i18n";
import { nameCollator } from "@/lib/utils";
import { useContext } from "react";
import { MeContext } from "@/lib/session";
import { useInterfaceStyle } from "@/lib/style";

// ───────────── Columns ─────────────

/** Details view columns besides the name, which is always shown. `date` is the list's main date (modified, or deleted in the trash) */
export type ColumnId = "location" | "date" | "created" | "type" | "size" | "owner" | "tags" | "extra";

/** Default widths in pixels; the name takes the space left, until it's resized itself */
export const COLUMN_WIDTH: Record<ColumnId, number> = { location: 220, date: 170, created: 170, type: 120, size: 100, owner: 110, tags: 150, extra: 110 };
export const MIN_COLUMN = 50;
export const MIN_NAME = 160;
export const MAX_COLUMN = 1000;

/** Columns that make room, in this order, when the list is too narrow for all of them (before it scrolls sideways) */
const GIVE_WAY: ColumnId[] = ["type", "size"];

/**
 * The columns to leave out so the others fit in `room` pixels, of which the name (and the check boxes) take `fixed`:
 * Type, then Size. When that isn't enough the list scrolls sideways
 */
export function columnsToHide(ids: ColumnId[], widthOf: (id: ColumnId) => number, fixed: number, room: number): ColumnId[] {
  let total = ids.reduce((sum, id) => sum + widthOf(id), fixed);
  const out: ColumnId[] = [];
  for (const id of GIVE_WAY) {
    if (total <= room) break;
    if (!ids.includes(id)) continue;
    total -= widthOf(id);
    out.push(id);
  }
  return out;
}

/** How many rows PageUp and PageDown move: those that fit in a view `height` high, less one so the last stays in sight (at least one) */
export function pageRows(height: number, rowHeight: number) {
  return Math.max(1, Math.floor(height / rowHeight) - 1);
}

/** Shown unless turned off; Date created and Tags are off until turned on */
const SHOWN: Record<ColumnId, boolean> = { location: true, date: true, created: false, type: true, size: true, owner: true, tags: false, extra: true };

export interface ColumnPrefs {
  visible: Partial<Record<ColumnId, boolean>>;
  widths: Partial<Record<ColumnId | "name", number>>;
}

const KEY = "tf-columns";
const prefs = createStore<ColumnPrefs>(load());
const scoped = new Map<string, typeof prefs>();

/** Windows keeps its existing preferences; Mac window columns belong to the current account. Public links need no session. */
export function useColumnScope() {
  const me = useContext(MeContext);
  const { style } = useInterfaceStyle();
  return style === "mac" ? `tf-columns-mac-${me?.id ?? "visitor"}` : KEY;
}

function storeFor(key: string) {
  if (key === KEY) return prefs;
  let store = scoped.get(key);
  if (!store) {
    store = createStore(load(key));
    scoped.set(key, store);
  }
  return store;
}

function load(key = KEY): ColumnPrefs {
  try {
    const raw = JSON.parse(localStorage.getItem(key) ?? localStorage.getItem(KEY) ?? "null") as Partial<ColumnPrefs> | null;
    return { visible: raw?.visible ?? {}, widths: raw?.widths ?? {} };
  } catch {
    return { visible: {}, widths: {} };
  }
}

function save(next: ColumnPrefs, key = KEY) {
  try {
    localStorage.setItem(key, JSON.stringify(next));
  } catch {
    // Ignore
  }
  storeFor(key).set(next);
}

export function columnPrefs() {
  return prefs.get();
}

/** The column settings, shared by every list (and kept for the next visit) */
export function useColumnPrefs(key = KEY) {
  return useStore(storeFor(key));
}

export function columnShown(p: ColumnPrefs, id: ColumnId, mac = false) {
  return p.visible[id] ?? (mac && id === "owner" ? false : SHOWN[id]);
}

export function showColumn(id: ColumnId, on: boolean, key = KEY) {
  const p = storeFor(key).get();
  save({ ...p, visible: { ...p.visible, [id]: on } }, key);
}

/** A column's width; undefined goes back to the default */
export function setColumnWidth(id: ColumnId | "name", width: number | undefined, key = KEY) {
  const p = storeFor(key).get();
  const widths = { ...p.widths };
  if (width === undefined) delete widths[id];
  else widths[id] = Math.round(Math.min(MAX_COLUMN, Math.max(id === "name" ? MIN_NAME : MIN_COLUMN, width)));
  save({ ...p, widths }, key);
}

/** Every column shown as it is at first, at its default width */
export function resetColumns(key = KEY) {
  save({ visible: {}, widths: {} }, key);
}

// ───────────── Groups ─────────────

export type GroupBy = "none" | "type" | "date";

export interface Group<T> {
  key: string;
  label: string;
  items: T[];
}

/** Date groups, newest first (as File Explorer names them) */
const DATE_GROUPS = ["today", "yesterday", "week", "last-week", "month", "last-month", "year", "older"] as const;
export type DateGroup = (typeof DATE_GROUPS)[number];

const DATE_LABEL: Record<DateGroup, string> = {
  today: t("Today"),
  yesterday: t("Yesterday"),
  week: t("Earlier this week"),
  "last-week": t("Last week"),
  month: t("Earlier this month"),
  "last-month": t("Last month"),
  year: t("Earlier this year"),
  older: t("A long time ago"),
};

/** Which date group a time (Unix seconds) falls in, seen from `now` in local time; weeks start on Monday. Times after now count as today */
export function dateGroup(time: number, now: Date): DateGroup {
  const day = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const at = new Date(time * 1000);
  const today = day(now);
  const daysAgo = Math.round((today - day(at)) / 86_400_000);
  if (daysAgo <= 0) return "today";
  if (daysAgo === 1) return "yesterday";
  const sinceMonday = (now.getDay() + 6) % 7;
  if (daysAgo <= sinceMonday) return "week";
  if (daysAgo <= sinceMonday + 7) return "last-week";
  if (at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth()) return "month";
  const lastMonth = new Date(now.getFullYear(), now.getMonth() - 1, 1);
  if (at.getFullYear() === lastMonth.getFullYear() && at.getMonth() === lastMonth.getMonth()) return "last-month";
  if (at.getFullYear() === now.getFullYear()) return "year";
  return "older";
}

/**
 * Items in groups, each keeping the list's order: by date (newest group first), or by type (folders first, then by the
 * type's name). `reversed` turns the groups around, when the list is sorted the other way by what it's grouped by.
 * Empty groups are left out; null when not grouped.
 */
export function groupItems<T extends { kind: string }>(items: T[], by: GroupBy, o: { dateOf(x: T): number; typeOf(x: T): string; now: Date; reversed?: boolean }): Group<T>[] | null {
  if (by === "none") return null;
  const groups = new Map<string, Group<T>>();
  for (const x of items) {
    const key = by === "date" ? dateGroup(o.dateOf(x), o.now) : x.kind === "folder" ? "" : o.typeOf(x);
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { key, label: by === "date" ? DATE_LABEL[key as DateGroup] : x.kind === "folder" ? o.typeOf(x) : key, items: [] }));
    g.items.push(x);
  }
  const rank = (g: Group<T>) => DATE_GROUPS.indexOf(g.key as DateGroup);
  const out = [...groups.values()].sort((a, b) => (by === "date" ? rank(a) - rank(b) : a.key === "" ? -1 : b.key === "" ? 1 : nameCollator.compare(a.key, b.key)));
  return o.reversed ? out.reverse() : out;
}
