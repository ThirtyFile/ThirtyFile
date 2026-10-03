/**
 * Selecting in a large folder that isn't all loaded. Select all, or Shift from one item to another with items between
 * them not loaded, selects a span: every item of the folder (or those from one item to another, in the folder's
 * order), less the ones left out with Ctrl. The browser doesn't need the ids of what the span holds: a change to the
 * selection asks the server for them a batch at a time (at most 1,000, what one change takes) and makes the change
 * batch after batch.
 */
import { toast } from "sonner";
import { api, type SortKey, type SortOrder } from "@/api";
import { t } from "@/lib/i18n";

/** An end of a span: the item, and where it was when selected */
export interface SpanEnd {
  id: string;
  index: number;
}

/** What the file list selects without having every item: the items from `from` to `to` (without them: from the first, to the last) */
export interface ListSpan {
  from?: SpanEnd;
  to?: SpanEnd;
  /** Left out with Ctrl */
  except: ReadonlySet<string>;
}

/** A span in a folder, in the order it was selected in */
export interface FolderSpan extends ListSpan {
  /** The folder's id, or a smart folder's as `smartListing` gives it */
  folder: string;
  sort: SortKey;
  order: SortOrder;
  /** How many items it held when selected */
  count: number;
}

/** What is selected: items one by one, and a span */
export interface Picked {
  ids: string[];
  span: FolderSpan | null;
  /** How many items that is */
  count: number;
}

/**
 * What a span of a smart folder (a saved search that lists like a folder) is selected in: "smart:<id>". A change to it
 * loads the smart folder's lists again as it does a folder's (lib/queries.ts), and no folder has such an id.
 */
export const smartListing = (id: number) => `smart:${id}`;

/** The smart folder a span is in; null for a folder */
export function smartOf(listing: string): number | null {
  return listing.startsWith("smart:") ? Number(listing.slice(6)) : null;
}

export function inSpan(span: ListSpan | null | undefined, index: number, id: string) {
  return !!span && index >= (span.from?.index ?? 0) && index <= (span.to?.index ?? Infinity) && !span.except.has(id);
}

/** How many items a span holds in a folder of `total` items */
export function spanCount(span: ListSpan, total: number) {
  const lo = span.from?.index ?? 0;
  const hi = Math.min(total - 1, span.to?.index ?? total - 1);
  return Math.max(0, hi - lo + 1 - span.except.size);
}

/** The most ids one change takes, and one answer of the server's /select holds */
export const SELECT_BATCH = 1000;

/** Batches of at most `size` ids */
export function chunks(ids: readonly string[], size = SELECT_BATCH): string[][] {
  const out: string[][] = [];
  for (let i = 0; i < ids.length; i += size) out.push(ids.slice(i, i + size));
  return out;
}

/** The ids a selection holds, a batch at a time: the items picked one by one, then those of the span, from the server */
export async function* batchesOf(picked: Picked): AsyncGenerator<string[]> {
  yield* chunks(picked.ids);
  const span = picked.span;
  if (!span) return;
  // An item picked one by one that the span holds too was already changed with its batch
  const done = new Set(picked.ids);
  let after: string | undefined;
  const smart = smartOf(span.folder);
  do {
    const req = { sort: span.sort, order: span.order, from: span.from?.id, to: span.to?.id, except: [...span.except], after };
    const page = smart !== null ? await api.smartSelection(smart, req) : await api.selection(span.folder, req);
    const ids = done.size ? page.ids.filter((id) => !done.has(id)) : page.ids;
    if (ids.length) yield ids;
    after = page.next ?? undefined;
  } while (after);
}

/**
 * Makes a change to every selected item, a batch at a time, showing how far it is when there is more than one batch.
 * `title` words the progress ("Moving…"). Resolves with how many items were done; throws what the first failed
 * batch threw (the batches before it are done).
 */
export async function eachBatch(picked: Picked, title: string, change: (ids: string[]) => Promise<unknown>): Promise<number> {
  const many = picked.count > SELECT_BATCH;
  const id = `batch-${Math.random()}`;
  let done = 0;
  try {
    for await (const ids of batchesOf(picked)) {
      if (many) toast.loading(t("{title} {done} of {total}", { title, done, total: picked.count }), { id, duration: Infinity });
      await change(ids);
      done += ids.length;
    }
  } finally {
    if (many) toast.dismiss(id);
  }
  return done;
}

/** At most `limit` ids of a selection (for a download or a ZIP file, which take them all at once); more say so */
export async function idsOf(picked: Picked, limit: number): Promise<string[]> {
  const out: string[] = [];
  for await (const ids of batchesOf(picked)) {
    out.push(...ids);
    // One more than allowed: the server says what the limit is
    if (out.length > limit) break;
  }
  return out;
}
