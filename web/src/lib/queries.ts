/**
 * What a change to files does to the lists and details already loaded. A change says what it touched (a `FileChange`),
 * and only the queries it can affect are updated: rows are changed or taken out in place where the server's answer
 * says how, and the lists whose order or membership can't be worked out here are loaded again. Lists not shown at the
 * moment are only marked out of date, and load again when they show.
 */
import type { Query, QueryClient, QueryKey } from "@tanstack/react-query";
import type { Node, NodeInfo } from "@/api";

/** Queries a change to files or folders can affect: lists, the opened item, space usage; not settings, users or branding */
const FILE_QUERIES = ["children", "node", "recent", "favorites", "tagged", "smart", "search", "shared-with-me", "trash", "drives", "me", "access", "versions"];

/**
 * What folders hold, counted through every level (the details pane): only changes that add, move or remove items
 * refresh it (`contents` of a change), not a rename or a new favorite
 */
export const FOLDER_CONTENTS = "folder-contents";

/** Lists of items (each has an id): folders' pages and the navigation pane's folders, and the lists of several places */
const LISTS = ["children", "recent", "favorites", "tagged", "smart", "search", "shared-with-me", "trash"];
/** Lists that show where each item is: a renamed or moved folder changes that */
const LOCATED = ["recent", "favorites", "tagged", "smart", "search", "shared-with-me"];

type Ids = Iterable<string | null | undefined>;

/** What a change to files touched */
export interface FileChange {
  /** Folders whose items changed in a way only the server can list (added, renamed, sorted elsewhere): loaded again */
  folders?: Ids;
  /** Folders whose items changed, marked out of date without loading them again while they show (a file being edited) */
  later?: Ids;
  /** Folders where whole folders arrived (an uploaded folder): their lists and every list below them already loaded */
  trees?: Ids;
  /** Items with new values, as the server answered (a rename, a save): changed in every list at once */
  updated?: (Partial<Node> & { id: string })[];
  /** Items whose details changed, without their new values (versions, history) */
  nodes?: Ids;
  /** Items gone (to the trash, or deleted): taken out of every list at once, with what was loaded inside them */
  removed?: Ids;
  /** Items moved into a folder: out of the other folders' lists, into that folder's, with their paths changed */
  moved?: { ids: Ids; to: string }[];
  /** Items whose access changed (people, groups, links): the access lists, "Shared with me" and the spaces */
  access?: Ids;
  /** Items the person gave up their access to (Leave): nothing loaded inside them is kept, and what shows them loads again */
  left?: Ids;
  /** Whole spaces changed (read-only, deleted, their storage): everything loaded from them */
  spaces?: Ids;
  /** What folders hold changed (items added, moved or removed): the details pane's counts */
  contents?: boolean;
  /** Space used changed: the space list and the person's own usage */
  usage?: boolean;
  favorites?: boolean;
  /** Tags put on or taken off items: the lists of tagged items (the rows' tags change with `updated`) */
  tags?: boolean;
  trash?: boolean;
  recent?: boolean;
  /** The reach isn't known: every file query, as before changes said what they touched */
  all?: boolean;
}

/** A rename, from the server's answer: the row changes at once, and its folder is put in order again */
export function renamed(node: Node): FileChange {
  return { updated: [{ id: node.id, name: node.name, mime: node.mime, updated_at: node.updated_at }], folders: [node.parent_id] };
}

/** New content saved or restored, from the server's answer (whether it is a favorite isn't in it: that stays as it was) */
export function saved(node: Node, list: "folders" | "later" = "folders"): FileChange {
  return { updated: [{ id: node.id, size: node.size, mime: node.mime, updated_at: node.updated_at }], [list]: [node.parent_id], recent: true, usage: true };
}

/** Refetch after renaming, moving, copying, deleting, restoring or uploading when the reach isn't known */
export function invalidateFiles(qc: QueryClient, ...more: string[]): Promise<unknown> {
  return Promise.all([...FILE_QUERIES, FOLDER_CONTENTS, ...more].map((key) => qc.invalidateQueries({ queryKey: [key] })));
}

// ───────────── Changes made at about the same time are applied together ─────────────

const SETS = ["folders", "later", "trees", "nodes", "removed", "access", "left", "spaces"] as const;
const FLAGS = ["contents", "usage", "favorites", "tags", "trash", "recent", "all"] as const;
/** Changes merged: ids in sets, and each moved item with where it went */
type Combined = { [K in (typeof SETS)[number]]: Set<string> } & { [K in (typeof FLAGS)[number]]: boolean } & {
  updated: Map<string, Partial<Node>>;
  moved: Map<string, string>;
};

interface Merged {
  change: Combined;
  done: (() => void)[];
  timer: ReturnType<typeof setTimeout>;
}
const pending = new WeakMap<QueryClient, Merged>();

const empty = (): Combined => ({
  folders: new Set(),
  later: new Set(),
  trees: new Set(),
  updated: new Map(),
  nodes: new Set(),
  removed: new Set(),
  moved: new Map(),
  access: new Set(),
  left: new Set(),
  spaces: new Set(),
  contents: false,
  usage: false,
  favorites: false,
  tags: false,
  trash: false,
  recent: false,
  all: false,
});

function add(into: Set<string>, ids: Ids | undefined) {
  for (const id of ids ?? []) if (id) into.add(id);
}

function merge(m: Combined, c: FileChange) {
  for (const k of SETS) add(m[k], c[k]);
  for (const u of c.updated ?? []) m.updated.set(u.id, { ...m.updated.get(u.id), ...u });
  for (const { ids, to } of c.moved ?? []) for (const id of ids) if (id) m.moved.set(id, to);
  for (const k of FLAGS) m[k] ||= !!c[k];
}

/**
 * Updates what a change touched (see `FileChange`). Changes asked for in the same moment (the files of an upload, the
 * steps of a batch) are applied together, once. Resolves when the lists shown have loaded again.
 */
export function refreshFiles(qc: QueryClient, change: FileChange): Promise<void> {
  let m = pending.get(qc);
  if (!m) {
    const merged: Merged = { change: empty(), done: [], timer: setTimeout(() => flush(qc, merged)) };
    pending.set(qc, merged);
    m = merged;
  }
  merge(m.change, change);
  const done = m.done;
  return new Promise((resolve) => done.push(resolve));
}

function flush(qc: QueryClient, m: Merged) {
  if (pending.get(qc) === m) pending.delete(qc);
  void apply(qc, m.change)
    .catch(() => undefined)
    .then(() => m.done.forEach((d) => d()));
}

// ───────────── Reading and changing the answers in the cache ─────────────

type Row = { id: string; parent_id?: string | null; kind?: string };
type Paged = { pages: { items: Row[] }[] };

/** The items of a cached answer: a list, the pages of a list, or search results */
export function rowsOf(data: unknown): Row[] | undefined {
  if (Array.isArray(data)) return data as Row[];
  if (!data || typeof data !== "object") return undefined;
  if ("pages" in data && Array.isArray((data as Paged).pages)) return (data as Paged).pages.flatMap((p) => p.items ?? []);
  if ("items" in data && Array.isArray((data as { items: unknown }).items)) return (data as { items: Row[] }).items;
  return undefined;
}

/** The same answer with its items changed (each list or page of it by `fn`) */
function withRows(data: unknown, fn: (items: Row[]) => Row[]): unknown {
  if (Array.isArray(data)) return fn(data as Row[]);
  if (data && typeof data === "object") {
    if ("pages" in data && Array.isArray((data as Paged).pages)) {
      const d = data as Paged;
      return { ...d, pages: d.pages.map((p) => (p.items ? { ...p, items: fn(p.items) } : p)) };
    }
    if ("items" in data && Array.isArray((data as { items: unknown }).items)) return { ...data, items: fn((data as { items: Row[] }).items) };
  }
  return data;
}

const idOf = (q: Query) => (typeof q.queryKey[1] === "string" ? q.queryKey[1] : undefined);
const isList = (q: Query) => LISTS.includes(q.queryKey[0] as string);
/** The navigation pane's list of a folder's folders */
const isTreeList = (q: Query) => q.queryKey[0] === "children" && q.queryKey[2] === "folders";

/**
 * Where each item known here is (item → its folder), from every list and opened item in the cache: enough to find the
 * lists loaded below a folder without asking the server
 */
function parentsKnown(qc: QueryClient): Map<string, string> {
  const up = new Map<string, string>();
  for (const q of qc.getQueryCache().getAll()) {
    const key = q.queryKey[0];
    if (key === "children") {
      const folder = idOf(q);
      for (const n of rowsOf(q.state.data) ?? []) if (folder) up.set(n.id, n.parent_id ?? folder);
    } else if (key === "node" && q.queryKey.length === 2) {
      const info = q.state.data as NodeInfo | undefined;
      if (!info?.path) continue;
      let parent: string | undefined = info.via_share ? undefined : info.drive.root_id;
      for (const c of info.path) {
        if (parent) up.set(c.id, parent);
        parent = c.id;
      }
    }
  }
  return up;
}

/** The folders of `roots`, and every folder known to be somewhere below one of them */
function below(qc: QueryClient, roots: Set<string>): Set<string> {
  if (!roots.size) return roots;
  const up = parentsKnown(qc);
  const out = new Set(roots);
  for (const start of up.keys()) {
    const seen: string[] = [];
    for (let id: string | undefined = start; id && seen.length < 256; id = up.get(id)) {
      if (out.has(id)) {
        seen.forEach((s) => out.add(s));
        break;
      }
      seen.push(id);
    }
  }
  return out;
}

/** An opened item's path (the folders above it) goes through one of `ids`, or it is one of them */
function pathTouches(q: Query, ids: Set<string>) {
  if (q.queryKey[0] !== "node") return false;
  const id = idOf(q);
  if (id && ids.has(id)) return true;
  const info = q.state.data as NodeInfo | undefined;
  return !!info?.path?.some((c) => ids.has(c.id));
}

/** Folders given by their aliases (a drop on "My files" or "All files") by their ids, as the lists are cached */
function resolveAliases(qc: QueryClient, c: Combined) {
  const me = qc.getQueryData<{ root_id: string | null; shared_root: string | null }>(["me"]);
  const alias = (id: string) => (id === "root" ? (me?.root_id ?? id) : id === "shared" ? (me?.shared_root ?? id) : id);
  for (const k of ["folders", "later", "trees"] as const) c[k] = new Set([...c[k]].map(alias));
  for (const [id, to] of c.moved) c.moved.set(id, alias(to));
}

async function apply(qc: QueryClient, c: Combined) {
  resolveAliases(qc, c);
  if (c.all) return void (await invalidateFiles(qc));
  const cache = qc.getQueryCache();
  // Each query found is marked once, however many parts of the change touch it (so it loads once)
  const now = new Set<Query>();
  const soon = new Set<Query>();
  const refetch = (predicate: (q: Query) => boolean) => cache.getAll().forEach((q) => predicate(q) && now.add(q));
  const later = (predicate: (q: Query) => boolean) => cache.getAll().forEach((q) => predicate(q) && soon.add(q));
  /** Forgets what isn't shown, and loads again what is (a list inside a folder that went to the trash) */
  const drop = (predicate: (q: Query) => boolean) => {
    qc.removeQueries({ predicate, type: "inactive" });
    refetch(predicate);
  };

  const moved = new Set(c.moved.keys());
  const gone = c.removed;
  // Answers on their way were asked for before the change: they are asked for again, so they don't bring old rows back
  const patched = new Set<Query>();

  // Rows changed or taken out in place: the lists stay where they were, and nothing is loaded for it
  if (gone.size || moved.size || c.updated.size) {
    for (const q of cache.findAll({ predicate: isList })) {
      const rows = rowsOf(q.state.data);
      if (!rows?.some((n) => gone.has(n.id) || moved.has(n.id) || c.updated.has(n.id))) continue;
      const folder = q.queryKey[0] === "children" ? idOf(q) : undefined;
      // A moved item leaves the lists of other folders; the lists of several places show it where it went (below)
      const leaves = (n: Row) => gone.has(n.id) || (!!folder && moved.has(n.id) && c.moved.get(n.id) !== folder);
      const next = withRows(q.state.data, (items) => items.filter((n) => !leaves(n)).map((n) => (c.updated.has(n.id) ? { ...n, ...c.updated.get(n.id) } : n)));
      // A part of a folder asked for by its position (lib/windows). One that holds the whole folder only counts one
      // less; in a larger folder the items after a row taken out move up, and only the server knows what comes in at
      // the end of each part
      const total = (q.state.data as { total?: unknown }).total;
      const whole = typeof total === "number" && rows.length === total;
      const out = rows.filter(leaves).length;
      qc.setQueryData(q.queryKey, whole && out ? { ...(next as object), total: total - out } : next);
      if (q.state.fetchStatus === "fetching" || (typeof total === "number" && !whole && out)) patched.add(q);
    }
    for (const [id, values] of c.updated) {
      qc.setQueryData<NodeInfo>(["node", id], (info) => (info?.node ? { ...info, node: { ...info.node, ...values } } : info));
      const q = cache.find({ queryKey: ["node", id], exact: true });
      if (q?.state.fetchStatus === "fetching") patched.add(q);
    }
  }
  if (patched.size) refetch((q) => patched.has(q));

  // Folders whose lists only the server can put in order again
  const folders = new Set(c.folders);
  for (const to of c.moved.values()) folders.add(to);
  const trees = below(qc, c.trees);
  trees.forEach((id) => folders.add(id));
  if (folders.size) {
    refetch((q) => q.queryKey[0] === "children" && folders.has(idOf(q) ?? ""));
    // The navigation pane shows an arrow on folders with folders in them: the lists showing these folders say that
    refetch((q) => isTreeList(q) && !!rowsOf(q.state.data)?.some((n) => folders.has(n.id)));
  }
  if (c.later.size) later((q) => q.queryKey[0] === "children" && c.later.has(idOf(q) ?? ""));
  // The activity of the folders changed (it lists what happens inside them) and of the items changed or moved
  const active = new Set([...folders, ...c.updated.keys(), ...c.moved.keys()]);
  if (active.size) refetch((q) => q.queryKey[0] === "node" && q.queryKey[2] === "history" && active.has(q.queryKey[1] as string));

  // Gone: what was loaded inside them, and their own details
  if (gone.size) {
    const inside = below(qc, gone);
    drop((q) => (q.queryKey[0] === "children" && inside.has(idOf(q) ?? "")) || pathTouches(q, gone));
    drop((q) => ["versions", "access", "shares"].includes(q.queryKey[0] as string) && gone.has(idOf(q) ?? ""));
  }

  // Paths that changed: moved items and everything in them, and what is inside renamed folders
  const renamed = new Set([...c.updated].filter(([, v]) => v.name !== undefined).map(([id]) => id));
  const repathed = new Set([...moved, ...renamed]);
  if (repathed.size) refetch((q) => pathTouches(q, repathed));
  // The lists of several places show where each item is, and can hold items at any depth inside what moved, was renamed
  // or went: they load again when shown (only the one shown, if any, loads now)
  if (repathed.size || gone.size) refetch((q) => LOCATED.includes(q.queryKey[0] as string));
  // Search results and smart folders (saved searches): new names and new items can match
  if (folders.size || repathed.size || trees.size) refetch((q) => q.queryKey[0] === "search" || q.queryKey[0] === "smart");

  // Details without new values: the item's info, its history and versions
  if (c.nodes.size) refetch((q) => (q.queryKey[0] === "node" || q.queryKey[0] === "versions") && c.nodes.has(idOf(q) ?? ""));

  if (c.left.size) {
    const inside = below(qc, c.left);
    drop((q) => (q.queryKey[0] === "children" && inside.has(idOf(q) ?? "")) || pathTouches(q, inside));
    // The lists that show these items: they may be gone from them now
    refetch((q) => isList(q) && !!rowsOf(q.state.data)?.some((n) => c.left.has(n.id)));
    c.left.forEach((id) => c.access.add(id));
  }
  if (c.access.size) {
    refetch((q) => q.queryKey[0] === "access" && c.access.has(idOf(q) ?? ""));
    // Roles below changed too
    refetch((q) => pathTouches(q, c.access));
    for (const key of ["drives", "shared-with-me"]) refetch((q) => q.queryKey[0] === key);
  }

  if (c.spaces.size) {
    const inSpace = (q: Query) => {
      if (q.queryKey[0] === "node") return c.spaces.has((q.state.data as NodeInfo | undefined)?.drive?.id ?? "");
      if (!isList(q)) return false;
      const rows = rowsOf(q.state.data);
      // An empty list says nothing of where it is: loaded again to be sure
      return !rows?.length || rows.some((n) => c.spaces.has((n as Partial<Node>).drive_id ?? ""));
    };
    refetch(inSpace);
    for (const key of ["drives", "me", "access"]) refetch((q) => q.queryKey[0] === key);
  }

  const flags: [boolean, string][] = [
    [c.contents || gone.size > 0 || moved.size > 0, FOLDER_CONTENTS],
    [c.usage, "drives"],
    [c.usage, "me"],
    [c.favorites, "favorites"],
    [c.tags, "tagged"],
    // Smart folders can look for tags
    [c.tags, "smart"],
    [c.trash || gone.size > 0, "trash"],
    [c.recent, "recent"],
  ];
  for (const [on, key] of flags) if (on) refetch((q) => q.queryKey[0] === key);
  await Promise.all([
    soon.size && qc.invalidateQueries({ predicate: (q) => soon.has(q) && !now.has(q), refetchType: "none" }),
    now.size && qc.invalidateQueries({ predicate: (q) => now.has(q) }),
  ]);
}

/**
 * A folder or item that can no longer be opened (403: access taken away; 404: gone): nothing loaded inside it stays in
 * the cache for someone who can't see it any more
 */
export function forgetUnreachable(qc: QueryClient, queryKey: QueryKey, status: number) {
  if (status !== 403 && status !== 404) return;
  const [kind, id] = queryKey;
  if ((kind !== "children" && kind !== "node") || typeof id !== "string") return;
  const inside = below(qc, new Set([id]));
  qc.removeQueries({ predicate: (q) => (q.queryKey[0] === "children" && inside.has(idOf(q) ?? "")) || pathTouches(q, inside), type: "inactive" });
}
