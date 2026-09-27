import { useSyncExternalStore } from "react";
import * as tus from "tus-js-client";
import { t, tServer } from "@/lib/i18n";

export type UploadStatus = "queued" | "uploading" | "paused" | "done" | "error";

export interface UploadTask {
  id: string;
  name: string;
  relativePath: string;
  parentId: string;
  /** Files added together, so the server puts one uploaded folder into one folder */
  batch: string;
  size: number;
  sent: number;
  status: UploadStatus;
  error?: string;
  upload?: tus.Upload;
  file: File;
}

export interface PickedFile {
  file: File;
  /** Relative folder path, e.g. "Photos/2024"; empty string for plain files */
  relativePath: string;
}

const CONCURRENCY = 3;
let tasks: UploadTask[] = [];
const listeners = new Set<() => void>();
const doneListeners = new Set<(parentId: string) => void>();
let seq = 0;

function emit() {
  tasks = [...tasks];
  listeners.forEach((l) => l());
}

function update(id: string, patch: Partial<UploadTask>) {
  tasks = tasks.map((t) => (t.id === id ? { ...t, ...patch } : t));
  listeners.forEach((l) => l());
}

export function useUploads() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => tasks,
  );
}

/** Notify when a file finishes uploading (used to refresh the list) */
export function onUploadDone(fn: (parentId: string) => void) {
  doneListeners.add(fn);
  return () => {
    doneListeners.delete(fn);
  };
}

function errorMessage(err: Error): string {
  const res = (err as tus.DetailedError).originalResponse;
  if (res) {
    try {
      const error: string | undefined = JSON.parse(res.getBody()).error;
      return error ? tServer(error) : err.message;
    } catch {
      return t("Upload failed ({status})", { status: res.getStatus() });
    }
  }
  return t("Network connection lost");
}

function start(task: UploadTask) {
  const upload = new tus.Upload(task.file, {
    endpoint: "/api/uploads",
    chunkSize: 32 * 1024 * 1024,
    retryDelays: [0, 1000, 3000, 5000, 10000, 20000],
    removeFingerprintOnSuccess: true,
    metadata: {
      filename: task.file.name,
      parentId: task.parentId,
      relativePath: task.relativePath,
      // Files of one uploaded folder land in the same folder, even when its name is taken by a file
      batchId: task.batch,
    },
    // Uploads of the same file to different locations must not resume each other
    fingerprint: async (file) =>
      ["sd", task.parentId, task.relativePath, (file as File).name, (file as File).size, (file as File).lastModified].join("|"),
    onProgress: (sent) => update(task.id, { sent }),
    onSuccess: () => {
      update(task.id, { status: "done", sent: task.size, upload: undefined });
      doneListeners.forEach((l) => l(task.parentId));
      pump();
    },
    onError: (err) => {
      update(task.id, { status: "error", error: errorMessage(err) });
      pump();
    },
    onShouldRetry: (err) => {
      const status = (err as tus.DetailedError).originalResponse?.getStatus() ?? 0;
      // Errors like permission denied, out of space or file too large don't need a retry
      return status === 0 || status === 409 || status === 423 || status >= 500;
    },
  });
  update(task.id, { status: "uploading", upload, error: undefined });
  upload.findPreviousUploads().then((previous) => {
    // The user may have paused or cancelled during the lookup: abort() has no effect yet, so check here before starting
    const current = tasks.find((x) => x.id === task.id);
    if (!current || current.status !== "uploading" || current.upload !== upload) return;
    if (previous.length > 0) upload.resumeFromPreviousUpload(previous[0]);
    upload.start();
  });
}

function pump() {
  const running = tasks.filter((t) => t.status === "uploading").length;
  const queued = tasks.filter((t) => t.status === "queued");
  for (const t of queued.slice(0, Math.max(0, CONCURRENCY - running))) start(t);
}

export function enqueue(files: PickedFile[], parentId: string) {
  // getRandomValues works on plain http too (randomUUID needs HTTPS)
  const batch = Array.from(crypto.getRandomValues(new Uint8Array(12)), (b) => b.toString(16).padStart(2, "0")).join("");
  for (const { file, relativePath } of files) {
    tasks.push({
      id: `u${++seq}`,
      name: file.name,
      relativePath,
      parentId,
      batch,
      size: file.size,
      sent: 0,
      status: "queued",
      file,
    });
  }
  emit();
  pump();
}

export function pause(id: string) {
  const t = tasks.find((x) => x.id === id);
  if (t?.status === "uploading") {
    t.upload?.abort();
    update(id, { status: "paused" });
    pump();
  }
}

export function resume(id: string) {
  const t = tasks.find((x) => x.id === id);
  if (t && (t.status === "paused" || t.status === "error")) {
    update(id, { status: "queued", error: undefined });
    pump();
  }
}

export function cancel(id: string) {
  const t = tasks.find((x) => x.id === id);
  if (!t) return;
  // abort(true) also tells the server to delete the temporary data
  if (t.upload && t.status !== "done") t.upload.abort(true).catch(() => {});
  tasks = tasks.filter((x) => x.id !== id);
  emit();
  pump();
}

export function clearFinished() {
  tasks = tasks.filter((t) => t.status !== "done");
  emit();
}

export function cancelAll() {
  for (const t of tasks) if (t.upload && t.status !== "done") t.upload.abort(true).catch(() => {});
  tasks = [];
  emit();
}

/** Get files from a drop event (including every file inside folders) */
export async function filesFromDrop(dt: DataTransfer): Promise<PickedFile[]> {
  const entries = Array.from(dt.items)
    .filter((i) => i.kind === "file")
    .map((i) => i.webkitGetAsEntry())
    .filter((e): e is FileSystemEntry => !!e);
  if (entries.length === 0) return Array.from(dt.files).map((file) => ({ file, relativePath: "" }));

  const out: PickedFile[] = [];
  const walk = async (entry: FileSystemEntry, dir: string): Promise<void> => {
    if (entry.isFile) {
      const file = await new Promise<File>((res, rej) => (entry as FileSystemFileEntry).file(res, rej));
      out.push({ file, relativePath: dir });
    } else if (entry.isDirectory) {
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      const path = dir ? `${dir}/${entry.name}` : entry.name;
      // readEntries returns at most 100 entries per call, so call it repeatedly
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((res, rej) => reader.readEntries(res, rej));
        if (batch.length === 0) break;
        for (const child of batch) await walk(child, path);
      }
    }
  };
  for (const e of entries) await walk(e, "");
  return out;
}

/** Get files from <input type=file webkitdirectory> */
export function filesFromInput(list: FileList): PickedFile[] {
  return Array.from(list).map((file) => {
    const rel = file.webkitRelativePath || "";
    const dir = rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/")) : "";
    return { file, relativePath: dir };
  });
}
