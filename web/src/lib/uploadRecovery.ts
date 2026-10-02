/**
 * Recovering interrupted uploads after a reload or a browser restart.
 *
 * The upload queue lives in memory (uploads.ts), with the files the person picked. What is needed to find an upload
 * again is kept in the browser's storage: the file's name, folder path, size, date and its complete content identity, where
 * it goes, its batch and the answer to a name clash, and how far it got. Never the content itself, nor anything that
 * signs in. The server session of an upload is found again the way tus-js-client finds it: its own records, keyed by
 * the upload's record (`fingerprintOf`), hold the upload's address, which for a share link's visitor includes the
 * link; they go when the upload ends, is discarded or expires with its record.
 *
 * Records are kept per signed-in person, or per share link for a link's visitors, so another account or link never
 * sees or continues them. Signing out removes them all (lib/signOut.ts). They expire after 7 days, when the server
 * has let the upload go too, and there are at most `MAX_RECORDS` per person or link.
 *
 * Each open tab refreshes the records of its uploads every few seconds. After a reload, the tab's own records show up
 * as interrupted at once; another tab's only once it stopped refreshing them (it was closed). The files must then be
 * chosen again: a page can't reopen them by itself.
 */

import { IDENTITY_PREFIX, identityOf } from "@/lib/identity";

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
  /** Complete content identity (`sampleOf`) checked before resuming; legacy records held sparse samples here. */
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

/** A short, stable hash (not for security): of a share link's address */
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

// ───────────── Content identity ─────────────

export { IDENTITY_PREFIX };

/** The worker that computes identities (null: it can't run here, so the page does it), and the answers it owes */
let worker: Worker | null | undefined;
const jobs = new Map<number, { file: Blob; resolve(identity: string): void; reject(err: Error): void }>();
let jobSeq = 0;

function hashWorker(): Worker | null {
  if (worker !== undefined) return worker;
  try {
    worker = typeof Worker === "undefined" ? null : new Worker(new URL("./identity.worker.ts", import.meta.url), { type: "module" });
  } catch {
    worker = null;
  }
  if (!worker) return null;
  worker.onmessage = (e: MessageEvent<{ id: number; identity?: string; error?: string }>) => {
    const job = jobs.get(e.data.id);
    jobs.delete(e.data.id);
    if (e.data.identity) job?.resolve(e.data.identity);
    else job?.reject(new Error(e.data.error ?? "failed"));
  };
  // A worker that can't start (blocked, or an old browser): what it was given is done by the page
  worker.onerror = () => {
    worker?.terminate();
    worker = null;
    for (const job of jobs.values()) identityOf(job.file).then(job.resolve, job.reject);
    jobs.clear();
  };
  return worker;
}

/**
 * The file's complete content identity (lib/identity.ts), computed in a worker so the page doesn't wait for it. Old
 * sparse samples cannot authorize a resume. `signal` stops it (the upload was cancelled).
 */
export function sampleOf(file: Blob, signal?: AbortSignal): Promise<string> {
  const w = hashWorker();
  if (!w) return identityOf(file, () => !!signal?.aborted);
  const id = ++jobSeq;
  return new Promise((resolve, reject) => {
    jobs.set(id, { file, resolve, reject });
    w.postMessage({ id, file });
    signal?.addEventListener("abort", () => {
      if (!jobs.delete(id)) return;
      w.postMessage({ id, stop: true });
      reject(new Error("stopped"));
    });
  });
}

// ───────────── Server sessions (tus-js-client's records) ─────────────

/**
 * The tus fingerprint of an upload: its record, so an upload only ever continues its own server session (a file chosen
 * again after a reload continues only once its content identity matched, `resumeRecovered`). The upload address is
 * hashed, so a share link isn't in the keys.
 */
export function fingerprintOf(endpoint: string, recordId: string) {
  return `${FINGERPRINT}${hash(endpoint)}|${recordId}`;
}
const FINGERPRINT = "tf|";

/**
 * Forgets tus-js-client's records that outlived the upload records (they expire together), and ends those of earlier
 * versions, whose keys held the upload address (their uploads can't be continued any more): when the page loads
 */
export function sweepSessions(now = Date.now()) {
  try {
    for (const key of Object.keys(localStorage)) {
      if (!key.startsWith("tus::")) continue;
      let kept: { uploadUrl?: unknown; creationTime?: unknown } = {};
      try {
        kept = JSON.parse(localStorage.getItem(key) ?? "{}");
      } catch {
        // Not readable: forgotten
      }
      if (!key.startsWith(`tus::${FINGERPRINT}`)) void endSession(key, kept.uploadUrl);
      else if (!(now - Date.parse(String(kept.creationTime)) < MAX_AGE)) localStorage.removeItem(key);
    }
  } catch {
    // Storage blocked: nothing kept
  }
}
sweepSessions();

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
export const hasSession = (endpoint: string, r: UploadRecord) => sessionsOf(fingerprintOf(endpoint, r.id)).length > 0;

/** Ends the server sessions of an upload and forgets them, so it can't be continued (discarded, or starting over) */
export async function forgetSessions(fingerprint: string) {
  for (const { key, url } of sessionsOf(fingerprint)) await endSession(key, url);
}

/** Forgets a server session kept under `key`, and lets the server drop what it received */
async function endSession(key: string, url: unknown) {
  try {
    localStorage.removeItem(key);
  } catch {
    // Blocked storage: nothing to forget
  }
  // Same origin only: the address comes from this site's own records
  if (typeof url !== "string" || (!url.startsWith("/") && !url.startsWith(location.origin))) return;
  // Kept alive: the address is forgotten already, so the request must reach the server even when the page goes away
  await fetch(url, { method: "DELETE", credentials: "same-origin", headers: { "Tus-Resumable": "1.0.0" }, keepalive: true }).catch(() => {});
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
