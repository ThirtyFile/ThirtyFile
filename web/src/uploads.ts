import { useMemo, useSyncExternalStore } from "react";
import * as tus from "tus-js-client";
import { toast } from "sonner";
import { ApiError, api, errorFromBody } from "@/api";
import { applyToUpload, resolveConflicts, topLevel } from "@/lib/conflicts";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";
import { createStore, useStore } from "@/lib/store";
import {
  BEAT_MS,
  TAB,
  fingerprintOf,
  forgetSessions,
  groupByBatch,
  interrupted,
  pathOf,
  recordId,
  recordsVersion,
  removeRecords,
  sampleOf,
  saveLive,
  scopeOf,
  sessionState,
  subscribeRecords,
  type RecoveredBatch,
  type UploadRecord,
} from "@/lib/uploadRecovery";

export type UploadStatus = "queued" | "uploading" | "paused" | "done" | "error";

export interface UploadTask {
  id: string;
  name: string;
  relativePath: string;
  parentId: string;
  /** Files added together, so the server puts one uploaded folder into one folder */
  batch: string;
  /** Where the upload is created: signed-in uploads, or a share link that accepts files */
  endpoint: string;
  size: number;
  sent: number;
  status: UploadStatus;
  error?: string;
  upload?: tus.Upload;
  file: File;
  /** When the name is taken: give that file the new content, or keep both (the new one gets a number) */
  onConflict: "replace" | "keep";
  /** The name the server gave the file, when it isn't the uploaded one ("Report (1).docx") */
  savedAs?: string;
  /** Its record in the browser's storage, to recover it after a reload (lib/uploadRecovery.ts); none without a scope */
  recordId: string;
  scope: string | null;
  created: number;
  /** A sample of the content, taken when it starts */
  sample?: string;
  /** Continued after a reload: how far it had got, and whether it must start again (the file changed, or "Start over") */
  recovered?: { sent: number; fresh: boolean };
  /** Start a new upload rather than continue one the server may have */
  fresh?: boolean;
  /** Lets go of this upload for other tabs (a Web Lock held while it is sent) */
  release?: () => void;
}

export interface PickedFile {
  file: File;
  /** Relative folder path, e.g. "Photos/2024"; empty string for plain files */
  relativePath: string;
  /** What the server does when the name is taken (keep both unless the user chose to replace) */
  onConflict?: "replace" | "keep";
}

/** Running totals, kept up to date as tasks change, so nothing has to add up every task */
export interface UploadTotals {
  size: number;
  sent: number;
  queued: number;
  uploading: number;
  paused: number;
  done: number;
  error: number;
}

export interface UploadsSnapshot {
  tasks: readonly UploadTask[];
  totals: UploadTotals;
}

const CONCURRENCY = 3;
/** Progress is shown at most this often, so thousands of small files don't re-render the page on every event */
const EMIT_MS = 250;
/** While files keep landing, lists are refreshed at most this often, and once more when the uploads end */
const LANDED_MS = 1500;

// Tasks are changed in place; the snapshot handed to React is rebuilt at most every EMIT_MS
let tasks: UploadTask[] = [];
const byId = new Map<string, UploadTask>();
const emptyTotals = (): UploadTotals => ({ size: 0, sent: 0, queued: 0, uploading: 0, paused: 0, done: 0, error: 0 });
let totals = emptyTotals();
const snapshot = createStore<UploadsSnapshot>({ tasks, totals: { ...totals } });
/** Tasks waiting to start, in order; entries no longer queued (cancelled, already started) are skipped */
let queue: UploadTask[] = [];
let queueHead = 0;
/** Where the files of the uploads that ended went (told with the final refresh) */
export interface LandedBatch {
  /** Folders files were uploaded to */
  folders: string[];
  /** Folders whole folders were uploaded to (their files are in subfolders made on the way) */
  trees: string[];
}
const landedListeners = new Set<(parentIds: string[], final: boolean, batch: LandedBatch) => void>();
const landedParents = new Set<string>();
/** Files landed since the last final refresh */
let landedAny = false;
const batch = { folders: new Set<string>(), trees: new Set<string>() };
let landedTimer: ReturnType<typeof setTimeout> | undefined;
let emitTimer: ReturnType<typeof setTimeout> | undefined;
let seq = 0;

function flush() {
  clearTimeout(emitTimer);
  emitTimer = undefined;
  snapshot.set({ tasks: [...tasks], totals: { ...totals } });
  schedulePersist();
}

// ───────────── Records for recovery after a reload (lib/uploadRecovery.ts) ─────────────

let persistTimer: ReturnType<typeof setTimeout> | undefined;
let beatTimer: ReturnType<typeof setInterval> | undefined;
/** Scopes this tab wrote records for, so a scope whose last upload ended gets its records removed */
let persisted = new Set<string>();

function schedulePersist() {
  persistTimer ??= setTimeout(persist, 1000);
}

const STATE: Record<Exclude<UploadStatus, "done">, UploadRecord["state"]> = { queued: "waiting", uploading: "sending", paused: "paused", error: "failed" };

/** Writes this tab's unfinished uploads to the browser's storage; while there are any, again every few seconds, so other tabs know they aren't interrupted */
function persist() {
  clearTimeout(persistTimer);
  persistTimer = undefined;
  const now = Date.now();
  const byScope = new Map<string, UploadRecord[]>();
  for (const task of tasks) {
    if (!task.scope || task.status === "done") continue;
    const list = byScope.get(task.scope) ?? [];
    list.push({
      id: task.recordId,
      name: task.name,
      relativePath: task.relativePath,
      parentId: task.parentId,
      batch: task.batch,
      size: task.size,
      lastModified: task.file.lastModified,
      onConflict: task.onConflict,
      sample: task.sample,
      sent: task.sent,
      state: STATE[task.status],
      error: task.error,
      created: task.created,
      updated: now,
      tab: TAB,
      beat: now,
    });
    byScope.set(task.scope, list);
  }
  for (const scope of new Set([...persisted, ...byScope.keys()])) saveLive(scope, byScope.get(scope) ?? []);
  persisted = new Set(byScope.keys());
  if (byScope.size && !beatTimer) beatTimer = setInterval(persist, BEAT_MS);
  else if (!byScope.size && beatTimer) {
    clearInterval(beatTimer);
    beatTimer = undefined;
  }
}

// Closing or reloading the page: the latest progress is kept
window.addEventListener("pagehide", () => {
  if (persisted.size || tasks.length) persist();
});

/** Holds a Web Lock on an upload while it is sent, so two tabs never send the same file; null when another tab has it */
function acquire(name: string): Promise<(() => void) | null> {
  const locks = (navigator as Navigator & { locks?: LockManager }).locks;
  if (!locks) return Promise.resolve(() => {});
  return new Promise((resolve) => {
    locks
      .request(name, { ifAvailable: true }, (lock) => {
        if (!lock) {
          resolve(null);
          return;
        }
        return new Promise<void>((release) => resolve(release));
      })
      .catch(() => resolve(() => {}));
  });
}

function releaseLock(task: UploadTask) {
  task.release?.();
  task.release = undefined;
}

/** The ids of the records of this tab's unfinished uploads */
function liveRecordIds() {
  return new Set(tasks.filter((x) => x.status !== "done").map((x) => x.recordId));
}

/**
 * Uploads to `endpoint` that were interrupted (a reload, a closed tab or browser), grouped as they were added; they
 * continue once their files are chosen again (`resumeRecovered`)
 */
export function useInterrupted(endpoint: string): RecoveredBatch[] {
  const version = useSyncExternalStore(subscribeRecords, recordsVersion);
  const { tasks: current } = useUploads();
  return useMemo(() => {
    const scope = scopeOf(endpoint);
    return scope ? groupByBatch(interrupted(scope, liveRecordIds())) : [];
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- recomputed when the records or the tasks change
  }, [endpoint, version, current]);
}

export interface ResumeResult {
  /** Files that continue where they stopped */
  continuing: number;
  /** Files that changed since, and start again */
  changed: number;
  /** Files that start again because that was asked for */
  restarted: number;
  /** Interrupted files that weren't among the chosen ones (they stay interrupted) */
  missing: number;
  /** Chosen files that weren't part of the interrupted upload (left out) */
  extra: number;
}

/**
 * Continues interrupted uploads with the files chosen again. A file is matched by its folder path and name, then must
 * have the same size, date and content sample; one that doesn't starts a new upload, and the part sent before is
 * dropped. The destination, batch and answer to a name clash stay as they were. `restart` starts them all again.
 */
export async function resumeRecovered(endpoint: string, records: UploadRecord[], picked: PickedFile[], restart = false): Promise<ResumeResult> {
  const byPath = new Map(picked.map((p) => [pathOf(p.relativePath, p.file.name), p]));
  const used = new Set<PickedFile>();
  const result: ResumeResult = { continuing: 0, changed: 0, restarted: 0, missing: 0, extra: 0 };
  const inits: TaskInit[] = [];
  for (const r of records) {
    const p = byPath.get(pathOf(r.relativePath, r.name));
    if (!p || used.has(p)) {
      result.missing++;
      continue;
    }
    used.add(p);
    const same = p.file.size === r.size && p.file.lastModified === r.lastModified && (!r.sample || (await sampleOf(p.file).catch(() => "")) === r.sample);
    const fresh = restart || !same;
    // The old upload can't be continued with this file: let the server drop what it received
    if (fresh) await forgetSessions(fingerprintOf(endpoint, r));
    if (!same) result.changed++;
    else if (restart) result.restarted++;
    else result.continuing++;
    inits.push({ file: p.file, relativePath: r.relativePath, parentId: r.parentId, batch: r.batch, onConflict: r.onConflict, recordId: r.id, recovered: { sent: fresh ? 0 : r.sent, fresh } });
  }
  result.extra = picked.length - used.size;
  addTasks(inits, endpoint);
  return result;
}

/** Drops interrupted uploads: their records, and what the server received of them */
export async function discardRecovered(endpoint: string, records: UploadRecord[]) {
  const scope = scopeOf(endpoint);
  if (scope) removeRecords(scope, records.map((r) => r.id));
  for (const r of records) await forgetSessions(fingerprintOf(endpoint, r));
}

/** Show changes: right away after something the user did, otherwise at most every EMIT_MS */
function emit(now = false) {
  if (now) flush();
  else emitTimer ??= setTimeout(flush, EMIT_MS);
}

function setStatus(task: UploadTask, status: UploadStatus) {
  totals[task.status]--;
  totals[status]++;
  task.status = status;
}

function setSent(task: UploadTask, sent: number) {
  totals.sent += sent - task.sent;
  task.sent = sent;
}

function forget(task: UploadTask) {
  totals[task.status]--;
  totals.size -= task.size;
  totals.sent -= task.sent;
  byId.delete(task.id);
}

export function useUploads() {
  return useStore(snapshot);
}

/** True while files are waiting or being sent (leaving the page would stop them) */
export function hasActiveUploads() {
  return totals.queued + totals.uploading > 0;
}

/**
 * Notify when uploaded files have landed, to refresh the lists: with the folders they were uploaded to (not for the
 * files of an uploaded folder), at most every LANDED_MS while uploads run, and once with final = true when nothing is
 * left to send, with every folder the uploads since the last final call went to (`batch`)
 */
export function onUploadsLanded(fn: (parentIds: string[], final: boolean, batch: LandedBatch) => void) {
  landedListeners.add(fn);
  return () => {
    landedListeners.delete(fn);
  };
}

function flushLanded() {
  clearTimeout(landedTimer);
  landedTimer = undefined;
  const final = !hasActiveUploads();
  if (landedParents.size === 0 && !(final && landedAny)) return;
  const ids = [...landedParents];
  landedParents.clear();
  const ended: LandedBatch = { folders: [...batch.folders], trees: [...batch.trees] };
  if (final) {
    landedAny = false;
    batch.folders.clear();
    batch.trees.clear();
  }
  landedListeners.forEach((l) => l(ids, final, ended));
}

function landed(task: UploadTask) {
  // A file of an uploaded folder lands in a subfolder, which the refresh when the uploads end shows
  if (!task.relativePath) landedParents.add(task.parentId);
  (task.relativePath ? batch.trees : batch.folders).add(task.parentId);
  landedAny = true;
  if (!hasActiveUploads()) flushLanded();
  else landedTimer ??= setTimeout(flushLanded, LANDED_MS);
}

function uploadError(err: Error): ApiError {
  const res = (err as tus.DetailedError).originalResponse;
  // Handled like api.request: the server's message translated, and an expired session sends the user to sign in
  if (res)
    return errorFromBody(
      res.getStatus(),
      res.getBody() ?? "",
      "/api/uploads",
      t("Upload failed ({status})", { status: res.getStatus() }),
      res.getHeader("x-request-id") || undefined,
    );
  return new ApiError(t("Network connection lost"), 0);
}

/** The name the server reports (percent-encoded), when it differs from the one uploaded */
export function savedName(header: string | undefined, uploaded: string): string | undefined {
  if (!header) return undefined;
  try {
    const name = decodeURIComponent(header);
    return name === uploaded ? undefined : name;
  } catch {
    return undefined;
  }
}

function start(task: UploadTask) {
  const fingerprint = fingerprintOf(task.endpoint, { ...task, lastModified: task.file.lastModified });
  const upload = new tus.Upload(task.file, {
    endpoint: task.endpoint,
    chunkSize: 32 * 1024 * 1024,
    retryDelays: [0, 1000, 3000, 5000, 10000, 20000],
    removeFingerprintOnSuccess: true,
    metadata: {
      filename: task.file.name,
      parentId: task.parentId,
      relativePath: task.relativePath,
      // Files of one uploaded folder land in the same folder, even when its name is taken by a file
      batchId: task.batch,
      onConflict: task.onConflict,
    },
    // Uploads of the same file to different locations, or with a different answer to a name clash, must not resume each other
    fingerprint: async () => fingerprint,
    onProgress: (sent) => {
      if (task.upload !== upload || task.status !== "uploading") return;
      setSent(task, sent);
      emit();
    },
    onSuccess: ({ lastResponse }) => {
      if (task.upload !== upload || task.status !== "uploading") return;
      task.savedAs = savedName(lastResponse.getHeader("x-node-name"), task.name);
      setSent(task, task.size);
      setStatus(task, "done");
      task.upload = undefined;
      releaseLock(task);
      pump();
      emit();
      landed(task);
    },
    onError: (err) => {
      if (task.upload !== upload || task.status !== "uploading") return;
      const e = uploadError(err);
      reportShown("upload", e, task.parentId);
      fail(task, e.message);
    },
    onShouldRetry: (err) => {
      const status = (err as tus.DetailedError).originalResponse?.getStatus() ?? 0;
      // Errors like permission denied, out of space or file too large don't need a retry
      return status === 0 || status === 409 || status === 423 || status >= 500;
    },
  });
  setStatus(task, "uploading");
  task.upload = upload;
  task.error = undefined;
  void begin(task, upload, fingerprint);
}

/** A task failed: it says why, and lets go of its file for other tabs */
function fail(task: UploadTask, message: string) {
  setStatus(task, "error");
  task.error = message;
  task.upload = undefined;
  releaseLock(task);
  pump();
  emit();
  if (!hasActiveUploads()) flushLanded();
}

/**
 * Starts sending: takes the file's lock (another tab may be sending it), notes a sample of its content, and continues
 * the upload the server has, if any. An upload continued after a reload asks the server first: one it finished (the
 * answer was lost) is done, and one it no longer has starts again, unless everything had been sent, which is then
 * reported rather than sent a second time.
 */
async function begin(task: UploadTask, upload: tus.Upload, fingerprint: string) {
  // The user may pause or cancel meanwhile: abort() has no effect before start(), so check after each step
  const current = () => byId.get(task.id) === task && task.status === "uploading" && task.upload === upload;
  const release = await acquire(`tf-upload:${task.recordId}`);
  if (!release) {
    if (current()) fail(task, t("This file is being uploaded in another tab"));
    return;
  }
  releaseLock(task);
  task.release = release;
  if (!current()) return releaseLock(task);
  task.sample ??= await sampleOf(task.file).catch(() => undefined);
  let previous = await upload.findPreviousUploads().catch(() => []);
  if (!current()) return releaseLock(task);
  const recovered = task.recovered;
  task.recovered = undefined;
  if (recovered && !recovered.fresh) {
    const state = previous.length ? await sessionState(previous[0].uploadUrl ?? "") : null;
    if (!current()) return releaseLock(task);
    if (state?.kind === "finished") {
      await forgetSessions(fingerprint);
      task.savedAs = savedName(state.savedAs ?? undefined, task.name);
      setSent(task, task.size);
      setStatus(task, "done");
      task.upload = undefined;
      releaseLock(task);
      pump();
      emit();
      landed(task);
      return;
    }
    if ((state === null || state.kind === "gone") && task.size > 0 && recovered.sent >= task.size) {
      // Everything had been sent and the server no longer knows it: it may have made the file already
      task.fresh = true;
      return fail(task, t("This file may already have been uploaded: check the folder, then retry to upload it again"));
    }
    if (state?.kind === "gone") previous = [];
  }
  if (task.fresh || recovered?.fresh) {
    await forgetSessions(fingerprint);
    task.fresh = false;
    previous = [];
    if (!current()) return releaseLock(task);
  }
  if (previous.length > 0) upload.resumeFromPreviousUpload(previous[0]);
  upload.start();
}

function pump() {
  while (totals.uploading < CONCURRENCY && queueHead < queue.length) {
    const next = queue[queueHead++];
    if (next.status === "queued" && byId.get(next.id) === next) start(next);
  }
  // Now and then drop the part of the queue already taken
  if (queueHead > 1024 && queueHead * 2 > queue.length) {
    queue = queue.slice(queueHead);
    queueHead = 0;
  }
}

function requeue(task: UploadTask) {
  setStatus(task, "queued");
  task.error = undefined;
  queue.push(task);
}

/** What a new task needs */
interface TaskInit {
  file: File;
  relativePath: string;
  parentId: string;
  batch: string;
  onConflict: "replace" | "keep";
  recordId?: string;
  recovered?: { sent: number; fresh: boolean };
}

/** Queues files for upload into a folder; `endpoint` is a share link's upload address for visitors of the link */
export function enqueue(files: PickedFile[], parentId: string, endpoint = "/api/uploads") {
  // getRandomValues works on plain http too (randomUUID needs HTTPS)
  const batch = Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  addTasks(
    files.map(({ file, relativePath, onConflict }) => ({ file, relativePath, parentId, batch, onConflict: onConflict ?? "keep" })),
    endpoint,
  );
}

function addTasks(inits: TaskInit[], endpoint: string) {
  if (!inits.length) return;
  const scope = scopeOf(endpoint);
  const now = Date.now();
  for (const { file, relativePath, parentId, batch, onConflict, recordId: id, recovered } of inits) {
    const task: UploadTask = {
      id: `u${++seq}`,
      name: file.name,
      relativePath,
      parentId,
      batch,
      endpoint,
      size: file.size,
      sent: 0,
      status: "queued",
      file,
      onConflict,
      recordId: id ?? recordId(),
      scope,
      created: now,
      recovered,
    };
    tasks.push(task);
    byId.set(task.id, task);
    totals.queued++;
    totals.size += task.size;
    queue.push(task);
  }
  pump();
  emit(true);
  // Recorded right away, so a reload in the next second still finds them
  persist();
}

/**
 * Upload picked or dropped files into a folder, asking first about names the folder already has (replace, skip or
 * keep both, as in Windows). Nothing is uploaded when the question is cancelled.
 */
export async function uploadFiles(files: PickedFile[], parentId: string) {
  if (!files.length) return;
  const tops = topLevel(files);
  let found;
  try {
    found = await api.conflicts({ dest_id: parentId, names: tops.map((x) => x.name) });
  } catch (e) {
    toast.error(e instanceof Error ? e.message : t("Couldn't upload"));
    reportShown("upload", e, parentId);
    return;
  }
  const byName = new Map(tops.map((x) => [x.name, x]));
  const answers = await resolveConflicts(
    found.map((c) => {
      const top = byName.get(c.name);
      return {
        key: c.name,
        name: c.name,
        kind: top?.kind ?? "file",
        size: top?.size,
        modified: top?.modified,
        existing: { kind: c.existing.kind, size: c.existing.size, updated_at: c.existing.updated_at },
      };
    }),
    "upload",
  );
  if (!answers) return;
  enqueue(applyToUpload(files, answers), parentId);
}

export function pause(id: string) {
  const t = byId.get(id);
  if (t?.status === "uploading") {
    t.upload?.abort();
    releaseLock(t);
    setStatus(t, "paused");
    pump();
    emit(true);
  }
}

export function resume(id: string) {
  const t = byId.get(id);
  if (t && (t.status === "paused" || t.status === "error")) {
    requeue(t);
    pump();
    emit(true);
  }
}

/** Try every failed upload again */
export function retryFailed() {
  for (const t of tasks) if (t.status === "error") requeue(t);
  pump();
  emit(true);
}

export function cancel(id: string) {
  const t = byId.get(id);
  if (!t) return;
  // abort(true) also tells the server to delete the temporary data
  if (t.upload && t.status !== "done") t.upload.abort(true).catch(() => {});
  t.upload = undefined;
  releaseLock(t);
  // Not started yet, or failed: what the server may have of it goes too
  if (t.status === "queued" || t.status === "error" || t.status === "paused") void forgetSessions(fingerprintOf(t.endpoint, { ...t, lastModified: t.file.lastModified }));
  forget(t);
  tasks = tasks.filter((x) => x !== t);
  pump();
  emit(true);
  if (!hasActiveUploads()) flushLanded();
}

export function clearFinished() {
  tasks = tasks.filter((t) => {
    if (t.status !== "done") return true;
    forget(t);
    return false;
  });
  emit(true);
}

export function cancelAll() {
  for (const t of tasks) {
    if (t.upload && t.status !== "done") t.upload.abort(true).catch(() => {});
    releaseLock(t);
  }
  tasks = [];
  byId.clear();
  totals = emptyTotals();
  queue = [];
  queueHead = 0;
  emit(true);
  persist();
  flushLanded();
}

// While files wait or are being sent, warn before closing or reloading the page: the files would have to be picked
// again to continue
window.addEventListener("beforeunload", (e) => {
  if (hasActiveUploads()) e.preventDefault();
});

/**
 * Get files from a drop event (including every file inside folders). Files and folders the browser can't read (no
 * permission, removed meanwhile, a broken link) are skipped, and a message says how many.
 */
export async function filesFromDrop(dt: DataTransfer): Promise<PickedFile[]> {
  const entries = Array.from(dt.items)
    .filter((i) => i.kind === "file")
    .map((i) => i.webkitGetAsEntry())
    .filter((e): e is FileSystemEntry => !!e);
  if (entries.length === 0) return Array.from(dt.files).map((file) => ({ file, relativePath: "" }));
  const { files, skipped } = await readEntries(entries);
  if (skipped) toast.error(t("{n} dropped item couldn't be read and was skipped.|{n} dropped items couldn't be read and were skipped.", { n: skipped }));
  return files;
}

/** Every file of dropped entries, walking into folders; `skipped` counts the files and folders that couldn't be read */
export async function readEntries(entries: FileSystemEntry[]): Promise<{ files: PickedFile[]; skipped: number }> {
  const files: PickedFile[] = [];
  let skipped = 0;
  const walk = async (entry: FileSystemEntry, dir: string): Promise<void> => {
    if (entry.isFile) {
      try {
        const file = await new Promise<File>((res, rej) => (entry as FileSystemFileEntry).file(res, rej));
        files.push({ file, relativePath: dir });
      } catch {
        skipped++;
      }
    } else if (entry.isDirectory) {
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      const path = dir ? `${dir}/${entry.name}` : entry.name;
      // readEntries returns at most 100 entries per call, so call it repeatedly
      for (;;) {
        let batch: FileSystemEntry[];
        try {
          batch = await new Promise<FileSystemEntry[]>((res, rej) => reader.readEntries(res, rej));
        } catch {
          // The rest of this folder can't be listed: what was read of it is still uploaded
          skipped++;
          break;
        }
        if (batch.length === 0) break;
        for (const child of batch) await walk(child, path);
      }
    }
  };
  for (const e of entries) await walk(e, "");
  return { files, skipped };
}

/** Get files from <input type=file webkitdirectory> */
export function filesFromInput(list: FileList): PickedFile[] {
  return Array.from(list).map((file) => {
    const rel = file.webkitRelativePath || "";
    const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
    return { file, relativePath: dir };
  });
}
