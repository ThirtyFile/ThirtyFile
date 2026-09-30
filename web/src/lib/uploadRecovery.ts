/**
 * Recovering interrupted uploads after a reload or a browser restart.
 *
 * The upload queue lives in memory (uploads.ts), with the files the person picked. What is needed to find an upload
 * again is kept in the browser's storage: the file's name, folder path, size, date and a sample of its content, where
 * it goes, its batch and the answer to a name clash, and how far it got. Never the content itself, nor anything that
 * signs in: the server session of an upload is found again the way tus-js-client finds it (its own records, which
 * hold only the upload's address).
 *
 * Records are kept per signed-in person, or per share link for a link's visitors, so another account or link never
 * sees or continues them. Signing out removes them all (lib/signOut.ts). They expire after 7 days, when the server
 * has let the upload go too, and there are at most `MAX_RECORDS` per person or link.
 *
 * Each open tab refreshes the records of its uploads every few seconds. After a reload, the tab's own records show up
 * as interrupted at once; another tab's only once it stopped refreshing them (it was closed). The files must then be
 * chosen again: a page can't reopen them by itself.
 */

/** A file that was being uploaded, as kept in the browser's storage */
export interface UploadRecord {
  id: string;
  name: string;
  /** Folder path inside an uploaded folder ("Photos/2024"), empty for a file */
  relativePath: string;
  parentId: string;
  batch: string;
  size: number;
  lastModified: number;
  onConflict: "replace" | "keep";
  /** A sample of the content (`sampleOf`), once the upload started: a file changed since doesn't continue the old upload */
  sample?: string;
  /** Bytes the server had received, as far as this page knew */
  sent: number;
  state: "waiting" | "sending" | "paused" | "failed";
  error?: string;
  created: number;
  updated: number;
  /** The tab that has this upload, and when it last said so */
  tab: string;
  beat: number;
}

const PREFIX = "tf-upload-tasks-";
/** Records kept per person or link */
export const MAX_RECORDS = 5000;
/** The server keeps an unfinished upload for 7 days (upload.rs, UPLOAD_TTL) */
const MAX_AGE = 7 * 86_400_000;
/** A tab refreshes its records this often, and records not refreshed for `STALE` belong to no open tab */
export const BEAT_MS = 5_000;
export const STALE_MS = 20_000;

const TAB_KEY = "tf-upload-tabs";
const newId = () => Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("");

/**
 * This page, and the pages this browser tab showed before it (reloads, pages left and come back to): their records
 * are interrupted as soon as this page is there, without waiting for them to go stale
 */
export const TAB = newId();
const EARLIER_PAGES: ReadonlySet<string> = (() => {
  try {
    const earlier: unknown = JSON.parse(sessionStorage.getItem(TAB_KEY) ?? "[]");
    const list = Array.isArray(earlier) ? earlier.filter((x): x is string => typeof x === "string") : [];
    sessionStorage.setItem(TAB_KEY, JSON.stringify([...list, TAB].slice(-20)));
    return new Set(list);
  } catch {
    return new Set<string>();
  }
})();

export const recordId = newId;

let userId: number | null = null;

/** Who is signed in: their uploads' records are theirs alone */
export function setRecoveryUser(id: number | null) {
  userId = id;
  changed();
}

/** A short, stable hash (not for security): of a share link's address, and of content samples */
export function hash(text: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 2654435761);
    h2 = Math.imul(h2 ^ c, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (h2 >>> 0).toString(16).padStart(8, "0") + (h1 >>> 0).toString(16).padStart(8, "0");
}

/**
 * Whose records an upload address has: the signed-in person's for their own uploads, a share link's for its
 * visitors (by a hash of the address, so the link's token isn't kept); none when nobody is known
 */
export function scopeOf(endpoint: string): string | null {
  if (endpoint === "/api/uploads") return userId === null ? null : `u${userId}`;
  return `s${hash(endpoint)}`;
}

function valid(r: unknown): r is UploadRecord {
  const x = r as UploadRecord;
  return !!x && typeof x.id === "string" && typeof x.name === "string" && typeof x.parentId === "string" && typeof x.size === "number";
}

export function readRecords(scope: string): UploadRecord[] {
  try {
    const list = JSON.parse(localStorage.getItem(PREFIX + scope) ?? "[]");
    const cutoff = Date.now() - MAX_AGE;
    return Array.isArray(list) ? list.filter((r) => valid(r) && r.updated > cutoff) : [];
  } catch {
    return [];
  }
}

function writeRecords(scope: string, records: UploadRecord[]) {
  try {
    if (records.length) localStorage.setItem(PREFIX + scope, JSON.stringify(records.slice(-MAX_RECORDS)));
    else localStorage.removeItem(PREFIX + scope);
  } catch {
    // Storage full or blocked: recovery isn't offered for these, the uploads themselves go on
  }
}

/**
 * Stores this tab's uploads of one person or link (`live`: every upload of theirs not finished yet), keeping the
 * records of other tabs and those still waiting to be recovered
 */
export function saveLive(scope: string, live: UploadRecord[]) {
  const liveIds = new Set(live.map((r) => r.id));
  // This tab's records that are no longer live were finished, cancelled or discarded
  const others = readRecords(scope).filter((r) => !liveIds.has(r.id) && r.tab !== TAB);
  writeRecords(scope, [...others, ...live]);
  changed();
}

/** Removes records (discarded, or continued and finished) */
export function removeRecords(scope: string, ids: Iterable<string>) {
  const gone = new Set(ids);
  writeRecords(
    scope,
    readRecords(scope).filter((r) => !gone.has(r.id)),
  );
  changed();
}

/**
 * The uploads of this person or link that were interrupted: this tab's from before a reload, and those of tabs that
 * were closed. `live`: the ids this tab is uploading now.
 */
export function interrupted(scope: string, live: ReadonlySet<string>, now = Date.now()): UploadRecord[] {
  return readRecords(scope).filter((r) => !live.has(r.id) && r.tab !== TAB && (EARLIER_PAGES.has(r.tab) || now - r.beat > STALE_MS));
}

/** A group of interrupted uploads that were added together (one uploaded folder, or files picked at once) */
export interface RecoveredBatch {
  batch: string;
  records: UploadRecord[];
  /** The uploaded folder's name, when it was one */
  folder: string | null;
  size: number;
  sent: number;
}

export function groupByBatch(records: UploadRecord[]): RecoveredBatch[] {
  const groups = new Map<string, UploadRecord[]>();
  for (const r of records) groups.set(r.batch, [...(groups.get(r.batch) ?? []), r]);
  return [...groups].map(([batch, list]) => ({
    batch,
    records: list,
    folder: list.find((r) => r.relativePath)?.relativePath.split("/")[0] ?? null,
    size: list.reduce((n, r) => n + r.size, 0),
    sent: list.reduce((n, r) => n + Math.min(r.sent, r.size), 0),
  }));
}

/** Where a file sits in what was picked: its folder path and name */
export const pathOf = (relativePath: string, name: string) => (relativePath ? `${relativePath}/${name}` : name);

// ───────────── Content samples ─────────────

/** Bytes read from each place in the file, and how many places: 64 KB in all, read in a moment even from a slow disk */
const SAMPLE_BYTES = 4096;
const SAMPLES = 16;

/**
 * A sample of a file's content, with its size: the whole of a small file, and 4 KB from 16 places spread from the start
 * to the end of a larger one. Another file with the same name, size and date (a replaced file) differs in it, so an
 * upload doesn't continue with it. An edit that keeps the size and the date and falls between the places isn't seen:
 * editing a file changes its date.
 */
export async function sampleOf(file: Blob): Promise<string> {
  const whole = file.size <= SAMPLE_BYTES * SAMPLES;
  const at = whole ? [0] : Array.from({ length: SAMPLES }, (_, i) => Math.floor(((file.size - SAMPLE_BYTES) * i) / (SAMPLES - 1)));
  const parts: string[] = [String(file.size)];
  for (const start of at) {
    const bytes = new Uint8Array(await file.slice(start, start + (whole ? file.size : SAMPLE_BYTES)).arrayBuffer());
    let s = "";
    for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    parts.push(hash(s));
  }
  return hash(parts.join("|"));
}

// ───────────── Server sessions (tus-js-client's records) ─────────────

/** The tus fingerprint of an upload: uploads to other places, or with another answer to a name clash, don't resume each other */
export function fingerprintOf(endpoint: string, r: Pick<UploadRecord, "parentId" | "relativePath" | "onConflict" | "name" | "size" | "lastModified">) {
  return ["sd", endpoint, r.parentId, r.relativePath, r.onConflict, r.name, r.size, r.lastModified].join("|");
}

/** The upload addresses tus-js-client kept for a fingerprint, with their storage keys */
export function sessionsOf(fingerprint: string): { key: string; url: string }[] {
  const out: { key: string; url: string }[] = [];
  try {
    for (const key of Object.keys(localStorage)) {
      if (!key.startsWith(`tus::${fingerprint}::`)) continue;
      const url = JSON.parse(localStorage.getItem(key) ?? "{}").uploadUrl;
      if (typeof url === "string") out.push({ key, url });
    }
  } catch {
    // Nothing kept
  }
  return out;
}

/** Whether the server may still have part of this upload (tus-js-client kept its address) */
export const hasSession = (endpoint: string, r: UploadRecord) => sessionsOf(fingerprintOf(endpoint, r)).length > 0;

/** Ends the server sessions of an upload and forgets them, so it can't be continued (discarded, or starting over) */
export async function forgetSessions(fingerprint: string) {
  for (const { key, url } of sessionsOf(fingerprint)) {
    try {
      localStorage.removeItem(key);
    } catch {
      // Blocked storage: nothing to forget
    }
    // Same origin only: the address comes from this site's own records
    if (!url.startsWith("/") && !url.startsWith(location.origin)) continue;
    await fetch(url, { method: "DELETE", credentials: "same-origin", headers: { "Tus-Resumable": "1.0.0" } }).catch(() => {});
  }
}

/** What the server says about an upload it may have (a HEAD request, as tus-js-client makes before continuing) */
export type SessionState =
  | { kind: "none" }
  | { kind: "partial"; offset: number }
  | { kind: "finished"; nodeId: string; savedAs: string | null }
  | { kind: "gone" }
  | { kind: "refused"; status: number; body: string };

export async function sessionState(url: string): Promise<SessionState> {
  let res: Response;
  try {
    res = await fetch(url, { method: "HEAD", credentials: "same-origin", headers: { "Tus-Resumable": "1.0.0" }, cache: "no-store" });
  } catch {
    // Offline: tus-js-client will retry
    return { kind: "none" };
  }
  if (res.ok) {
    const nodeId = res.headers.get("x-node-id");
    if (nodeId) return { kind: "finished", nodeId, savedAs: res.headers.get("x-node-name") };
    return { kind: "partial", offset: Number(res.headers.get("upload-offset")) || 0 };
  }
  if (res.status === 404 || res.status === 410) return { kind: "gone" };
  return { kind: "refused", status: res.status, body: "" };
}

// ───────────── Clearing ─────────────

/**
 * Removes the records of everyone but `keep` (a person's scope), and with `keep` null all of them: when someone else
 * signs in, and when signing out
 */
export function forgetRecords(keep: string | null, alsoLinks: boolean) {
  try {
    for (const key of Object.keys(localStorage)) {
      if (!key.startsWith(PREFIX)) continue;
      const scope = key.slice(PREFIX.length);
      if (scope === keep) continue;
      if (scope.startsWith("s") && !alsoLinks) continue;
      localStorage.removeItem(key);
    }
  } catch {
    // Storage blocked: nothing was kept
  }
  changed();
}

// ───────────── Following changes ─────────────

const listeners = new Set<() => void>();
let version = 0;

function changed() {
  version++;
  listeners.forEach((l) => l());
}

/** Subscribe to changes of the records (this tab's, other tabs', and time passing, which makes records of closed tabs interrupted) */
export function subscribeRecords(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1) {
    window.addEventListener("storage", onStorage);
    timer = setInterval(changed, BEAT_MS);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      window.removeEventListener("storage", onStorage);
      clearInterval(timer);
    }
  };
}

let timer: ReturnType<typeof setInterval> | undefined;
const onStorage = (e: StorageEvent) => {
  if (e.key === null || e.key.startsWith(PREFIX)) changed();
};

export const recordsVersion = () => version;
