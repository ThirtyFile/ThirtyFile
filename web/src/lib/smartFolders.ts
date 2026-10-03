/**
 * Smart folders: each person's saved searches, which show as folders and list what matches whenever they are opened
 * (server/src/nodes/smart.rs). Only the person who saved one sees it. A smart folder holds no items, only what it looks
 * for: deleting one deletes nothing else, and moving an item never puts it in one.
 *
 * The dialog that makes or changes one (components/smartFolders.tsx) edits a `SmartForm`: the query as the dialog's
 * fields show it, with the search page's choices of type, date and size (lib/searchFilters.ts), or ranges of its own.
 */
import { useMemo } from "react";
import { useQuery, type QueryClient } from "@tanstack/react-query";
import { api, type SmartFolder, type SmartQuery, type SmartScope } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { DAY, MB, SEARCH_DATES, SEARCH_SIZES, SEARCH_TYPES } from "@/lib/searchFilters";
import { createStore } from "@/lib/store";
import { nameCollator } from "@/lib/utils";

/** Longest name the server takes */
export const MAX_SMART_NAME = 100;

export const smartPath = (folder: Pick<SmartFolder, "id">) => `/smart/${folder.id}`;

/** Smart folders in the order they are listed: by name, as the server sorts them */
export const byName = (a: SmartFolder, b: SmartFolder) => nameCollator.compare(a.name, b.name) || a.id - b.id;

/** The person's smart folders, by name, and each by its id */
export function useSmartFolders(enabled = true) {
  const q = useQuery({ ...queries.smartFolders, enabled });
  return useMemo(() => {
    const folders = q.data ?? [];
    return { folders, byId: new Map(folders.map((f) => [f.id, f])), loading: q.isLoading, error: q.error };
  }, [q.data, q.isLoading, q.error]);
}

/** Keeps the list of smart folders in order after a change the server answered with */
function keep(qc: QueryClient, change: (folders: SmartFolder[]) => SmartFolder[]) {
  qc.setQueryData<SmartFolder[]>(keys.smartFolders(), (folders) => (folders ? change(folders).sort(byName) : folders));
}

/** Saves a search as a smart folder (the server says why a name or query can't be used) */
export async function createSmartFolder(qc: QueryClient, name: string, query: SmartQuery): Promise<SmartFolder> {
  const folder = await api.createSmartFolder(name, query);
  keep(qc, (folders) => [...folders.filter((f) => f.id !== folder.id), folder]);
  return folder;
}

/** Renames a smart folder, or changes what it looks for: what it lists loads again */
export async function updateSmartFolder(qc: QueryClient, id: number, change: { name?: string; query?: SmartQuery }): Promise<SmartFolder> {
  const folder = await api.updateSmartFolder(id, change);
  keep(qc, (folders) => folders.map((f) => (f.id === folder.id ? folder : f)));
  if (change.query) await qc.resetQueries({ queryKey: keys.smart(id) });
  return folder;
}

/** Deletes a smart folder: only the saved search, never the items it lists */
export async function deleteSmartFolder(qc: QueryClient, id: number): Promise<void> {
  await api.deleteSmartFolder(id);
  keep(qc, (folders) => folders.filter((f) => f.id !== id));
  qc.removeQueries({ queryKey: keys.smart(id) });
}

// ───────────── The dialog ─────────────

/** The dialog that saves a smart folder (`draft`: from a search) or changes `folder`, shown by <SmartFolderDialogHost /> */
export interface SmartDialogRequest {
  folder?: SmartFolder;
  draft?: { name: string; query: SmartQuery };
  resolve(folder: SmartFolder | null): void;
}

export const smartDialog = createStore<SmartDialogRequest | null>(null);

/** Asks for a new smart folder (filled in from `draft`), or changes `folder`: the one saved, null when cancelled */
export function editSmartFolder(folder?: SmartFolder, draft?: SmartDialogRequest["draft"]): Promise<SmartFolder | null> {
  smartDialog.get()?.resolve(null);
  return new Promise((resolve) => smartDialog.set({ folder, draft, resolve }));
}

// ───────────── From a search ─────────────

/** What the search page is filtered by (its address): the term, the folder looked in, and the choices made */
export interface SearchChoices {
  term: string;
  within?: string;
  type?: string;
  date?: string;
  size?: string;
  tag?: string;
}

/** A search as a smart folder's query; a period ("Last 7 days") stays one, counted whenever the folder is opened */
export function queryFromSearch(s: SearchChoices): SmartQuery {
  const type = SEARCH_TYPES.find((x) => x.id === s.type)?.filter;
  const size = SEARCH_SIZES.find((x) => x.id === s.size)?.filter;
  const days = SEARCH_DATES.find((x) => x.id === s.date)?.days;
  const tag = Number(s.tag);
  return clean({
    name: s.term.trim() || undefined,
    kind: type?.kind,
    ext: type?.ext,
    min_size: size?.min_size,
    max_size: size?.max_size,
    modified_days: days,
    scope: s.within ? { kind: "folder", id: s.within } : { kind: "all" },
    tags: s.tag && Number.isInteger(tag) ? [tag] : undefined,
  });
}

/** Without the parts left empty */
function clean(q: SmartQuery): SmartQuery {
  return Object.fromEntries(Object.entries(q).filter(([, v]) => v !== undefined && v !== "" && !(Array.isArray(v) && !v.length))) as unknown as SmartQuery;
}

// ───────────── The dialog's fields ─────────────

/** The query as the dialog shows it */
export interface SmartForm {
  name: string;
  /** In the name */
  term: string;
  /** "all", "space:<id>" or "folder:<id>" */
  scope: string;
  /** "", "file", one of SEARCH_TYPES, or "custom" (`ext`) */
  type: string;
  ext: string;
  /** "", one of SEARCH_DATES, "days" (`days`, a period the choices don't have) or "range" (`from`, `to`) */
  date: string;
  days: number;
  /** yyyy-mm-dd, both days counted */
  from: string;
  to: string;
  /** "", one of SEARCH_SIZES, or "range" (`minMb`, `maxMb`) */
  size: string;
  minMb: string;
  maxMb: string;
  tags: number[];
}

export const scopeValue = (s: SmartScope) => (s.kind === "all" ? "all" : `${s.kind}:${s.id}`);

function scopeOf(value: string): SmartScope {
  const [kind, ...rest] = value.split(":");
  const id = rest.join(":");
  return kind === "space" || kind === "folder" ? { kind, id } : { kind: "all" };
}

/** A day (yyyy-mm-dd) as the Unix time it starts at, here */
export function dayStart(day: string): number | undefined {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!m) return undefined;
  return Math.floor(new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime() / 1000);
}

/** The day (yyyy-mm-dd, here) a Unix time is on */
export function dayOf(time: number): string {
  const d = new Date(time * 1000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

const mb = (bytes: number | undefined) => (bytes === undefined ? "" : String(Math.round((bytes / MB) * 100) / 100));

export function formOf(name: string, q: SmartQuery): SmartForm {
  const type = q.kind === "folder" && !q.ext ? "folder" : q.kind === "file" && !q.ext ? "file" : q.ext ? (SEARCH_TYPES.find((x) => x.filter.ext === q.ext)?.id ?? "custom") : "";
  const sizes = SEARCH_SIZES.find((x) => x.filter.min_size === q.min_size && x.filter.max_size === q.max_size);
  const size = q.min_size === undefined && q.max_size === undefined ? "" : (sizes?.id ?? "range");
  const preset = SEARCH_DATES.find((x) => x.days === q.modified_days);
  const date = q.modified_days ? (preset?.id ?? "days") : q.modified_from !== undefined || q.modified_to !== undefined ? "range" : "";
  return {
    name,
    term: q.name ?? "",
    scope: scopeValue(q.scope ?? { kind: "all" }),
    type,
    ext: type === "custom" ? (q.ext ?? "") : "",
    date,
    days: q.modified_days ?? 0,
    from: q.modified_from !== undefined ? dayOf(q.modified_from) : "",
    // The last day counted: the server's end is the start of the day after it
    to: q.modified_to !== undefined ? dayOf(q.modified_to - DAY / 2) : "",
    size,
    minMb: size === "range" ? mb(q.min_size) : "",
    maxMb: size === "range" ? mb(q.max_size) : "",
    tags: [...(q.tags ?? [])],
  };
}

const bytes = (text: string) => {
  const n = Number(text.trim().replace(",", "."));
  return text.trim() && Number.isFinite(n) ? Math.round(n * MB) : undefined;
};

/** The query the dialog's fields make */
export function queryOf(f: SmartForm): SmartQuery {
  const type = SEARCH_TYPES.find((x) => x.id === f.type)?.filter;
  const size = SEARCH_SIZES.find((x) => x.id === f.size)?.filter;
  const preset = SEARCH_DATES.find((x) => x.id === f.date);
  const to = f.date === "range" ? dayStart(f.to) : undefined;
  return clean({
    name: f.term.trim() || undefined,
    kind: f.type === "file" ? "file" : type?.kind,
    ext: f.type === "custom" ? f.ext.trim() || undefined : type?.ext,
    min_size: f.size === "range" ? bytes(f.minMb) : size?.min_size,
    max_size: f.size === "range" ? bytes(f.maxMb) : size?.max_size,
    modified_days: preset?.days ?? (f.date === "days" && f.days > 0 ? f.days : undefined),
    modified_from: f.date === "range" ? dayStart(f.from) : undefined,
    // Up to the end of the last day
    modified_to: to === undefined ? undefined : dayStart(dayOf(to + DAY + DAY / 2)),
    scope: scopeOf(f.scope),
    tags: f.tags,
  });
}
