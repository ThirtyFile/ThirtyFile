/**
 * The Columns view (components/columns): a column for each folder from the top of a location down to the open one, and
 * on to the right through the folders opened from it. That path is the trail. Going back to a folder on it (Left, the
 * parent's column, Back) keeps the columns to its right; opening another folder from a column replaces them.
 *
 * Each column can be made wider or narrower: the widths are kept in the browser for each person, by folder, and
 * removed when they sign out (lib/signOut.ts).
 */
import type { Crumb } from "@/api";

/** The folders the columns show, from the top of the location */
export type Trail = readonly Crumb[];

/**
 * The trail for a folder reached by `path` (from the top of its location down to it): the trail kept when it goes
 * through that folder, so the columns opened beyond it stay; otherwise the path itself
 */
export function trailFor(kept: Trail | null, path: Trail): Trail {
  if (!path.length) return kept ?? path;
  if (kept && path.length <= kept.length && path.every((c, i) => kept[i].id === c.id)) {
    // The names the path gives are the current ones
    return path.every((c, i) => kept[i].name === c.name) ? kept : [...path, ...kept.slice(path.length)];
  }
  return path;
}

/**
 * The trail once `next` is opened from the column at `depth` (null: no folder is opened from it): the columns after
 * that one go, unless `next` is the folder already open there
 */
export function openFrom(trail: Trail, depth: number, next: Crumb | null): Trail {
  const open = trail[depth + 1];
  if (next && open?.id === next.id) return open.name === next.name ? trail : [...trail.slice(0, depth + 1), next, ...trail.slice(depth + 2)];
  if (!next && trail.length === depth + 1) return trail;
  return next ? [...trail.slice(0, depth + 1), next] : trail.slice(0, depth + 1);
}

/** Where `id` is on the trail; -1 when it isn't */
export const depthOf = (trail: Trail, id: string | undefined) => (id === undefined ? -1 : trail.findIndex((c) => c.id === id));

/** The trail of the page, kept while it is open: a folder left and then returned to shows the same columns */
let kept: Trail | null = null;
export const keptTrail = () => kept;
export const keepTrail = (trail: Trail) => void (kept = trail);

/**
 * Going to another column opens its folder (the explorer shows one folder at a time): what happens on arriving there.
 * `select`: the item to select (the folder came from, or one clicked in that column); `first`: the first item instead;
 * neither: nothing. `focus`: the focus goes to it. `menuAt`: its menu opens there (it was right-clicked).
 */
export interface Arrival {
  folder: string;
  select?: string;
  first?: boolean;
  focus?: boolean;
  menuAt?: { x: number; y: number };
}

/** How long an arrival waits for its folder: one that didn't open (it failed, or something else was opened) is forgotten */
const ARRIVAL_MS = 15_000;
let arrival: (Arrival & { at: number }) | null = null;

export function arriveAt(a: Arrival) {
  arrival = { ...a, at: Date.now() };
}

/** Whether an item is to be selected on arriving in `folder` (the arrival isn't taken) */
export const selectsOnArrival = (folder: string | undefined) => !!arrival && arrival.folder === folder && (!!arrival.select || !!arrival.first);

/** The arrival planned in `folder`, once */
export function takeArrival(folder: string): Arrival | null {
  const a = arrival;
  if (!a || a.folder !== folder) return null;
  arrival = null;
  return Date.now() - a.at < ARRIVAL_MS ? a : null;
}

// ───────────── Widths ─────────────

/** A column's width in pixels, until it is resized; the preview column's; and the limits of both */
export const COLUMN_WIDTH = 240;
export const PREVIEW_WIDTH = 300;
export const MIN_COLUMN_WIDTH = 160;
export const MAX_COLUMN_WIDTH = 640;
/** The key of the preview column's width among the folders' */
export const PREVIEW = "preview";
/** Widths are kept for at most this many folders: those resized last */
export const KEPT_WIDTHS = 200;

/** Widths by folder id (and PREVIEW) */
export type ColumnWidths = Readonly<Record<string, number>>;

/** The widths with one changed (`undefined`: back to its usual width); the folder changed last goes to the end, and the oldest beyond KEPT_WIDTHS go */
export function withWidth(widths: ColumnWidths, id: string, width: number | undefined): ColumnWidths {
  const rest = Object.entries(widths).filter(([k]) => k !== id);
  if (width !== undefined) rest.push([id, Math.round(Math.min(MAX_COLUMN_WIDTH, Math.max(MIN_COLUMN_WIDTH, width)))]);
  return Object.fromEntries(rest.slice(-KEPT_WIDTHS));
}

/** Whether a stored value is widths (a number for each key) */
export const validWidths = (v: ColumnWidths) => Object.values(v).every((w) => typeof w === "number" && Number.isFinite(w));
