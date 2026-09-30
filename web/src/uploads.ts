import { useSyncExternalStore } from "react";
import * as tus from "tus-js-client";
import { toast } from "sonner";
import { ApiError, api, errorFromBody } from "@/api";
import { resolveConflicts } from "@/components/ConflictDialog";
import { applyToUpload, topLevel } from "@/lib/conflicts";
import { reportShown } from "@/lib/errorReport";
import { t } from "@/lib/i18n";

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
let snapshot: UploadsSnapshot = { tasks, totals: { ...totals } };
/** Tasks waiting to start, in order; entries no longer queued (cancelled, already started) are skipped */
let queue: UploadTask[] = [];
let queueHead = 0;
const listeners = new Set<() => void>();
const landedListeners = new Set<(parentIds: string[], final: boolean) => void>();
const landedParents = new Set<string>();
/** Files landed since the last final refresh */
let landedAny = false;
let landedTimer: ReturnType<typeof setTimeout> | undefined;
let emitTimer: ReturnType<typeof setTimeout> | undefined;
let seq = 0;

function flush() {
  clearTimeout(emitTimer);
  emitTimer = undefined;
  snapshot = { tasks: [...tasks], totals: { ...totals } };
  listeners.forEach((l) => l());
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
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => snapshot,
  );
}

/** True while files are waiting or being sent (leaving the page would stop them) */
export function hasActiveUploads() {
  return totals.queued + totals.uploading > 0;
}

/**
 * Notify when uploaded files have landed, to refresh the lists: with the folders they were uploaded to (not for the
 * files of an uploaded folder), at most every LANDED_MS while uploads run, and once with final = true when nothing is
 * left to send
 */
export function onUploadsLanded(fn: (parentIds: string[], final: boolean) => void) {
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
  if (final) landedAny = false;
  landedListeners.forEach((l) => l(ids, final));
}

function landed(task: UploadTask) {
  // A file of an uploaded folder lands in a subfolder, which the refresh when the uploads end shows
  if (!task.relativePath) landedParents.add(task.parentId);
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
    fingerprint: async (file) =>
      ["sd", task.endpoint, task.parentId, task.relativePath, task.onConflict, (file as File).name, (file as File).size, (file as File).lastModified].join("|"),
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
      pump();
      emit();
      landed(task);
    },
    onError: (err) => {
      if (task.upload !== upload || task.status !== "uploading") return;
      setStatus(task, "error");
      const e = uploadError(err);
      task.error = e.message;
      reportShown("upload", e, task.parentId);
      pump();
      emit();
      if (!hasActiveUploads()) flushLanded();
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
  upload.findPreviousUploads().then((previous) => {
    // The user may have paused or cancelled during the lookup: abort() has no effect yet, so check here before starting
    if (byId.get(task.id) !== task || task.status !== "uploading" || task.upload !== upload) return;
    if (previous.length > 0) upload.resumeFromPreviousUpload(previous[0]);
    upload.start();
  });
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

/** Queues files for upload into a folder; `endpoint` is a share link's upload address for visitors of the link */
export function enqueue(files: PickedFile[], parentId: string, endpoint = "/api/uploads") {
  // getRandomValues works on plain http too (randomUUID needs HTTPS)
  const batch = Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  for (const { file, relativePath, onConflict } of files) {
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
      onConflict: onConflict ?? "keep",
    };
    tasks.push(task);
    byId.set(task.id, task);
    totals.queued++;
    totals.size += task.size;
    queue.push(task);
  }
  pump();
  emit(true);
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
  for (const t of tasks) if (t.upload && t.status !== "done") t.upload.abort(true).catch(() => {});
  tasks = [];
  byId.clear();
  totals = emptyTotals();
  queue = [];
  queueHead = 0;
  emit(true);
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
