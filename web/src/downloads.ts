//! Download manager: downloads in the page with progress (name, percentage, speed, time left), then hands the file to the browser to save.
//! Multi-item downloads are zipped by the server while streaming, with the total size announced up front, so progress can be shown too.
//! Files larger than IN_APP_LIMIT aren't buffered in the page (to avoid using lots of memory); the browser downloads them directly instead.

import { useSyncExternalStore } from "react";
import { toast } from "sonner";
import { t, tServer } from "@/lib/i18n";

export type DownloadStatus = "downloading" | "done" | "error" | "canceled";

export interface DownloadTask {
  id: string;
  name: string;
  /** Zipped download (multiple items or folders) */
  zip: boolean;
  /** Total size; null when the server doesn't provide it */
  total: number | null;
  received: number;
  /** Recent download speed (bytes/second) */
  rate: number;
  status: DownloadStatus;
  error?: string;
  url: string;
  controller: AbortController;
}

/**
 * Limit for downloading in the page; larger files are handed to the browser to download directly.
 * Phones (touch devices) or computers with little memory use a lower limit, so the tab isn't killed by the system for running out of memory
 */
const IN_APP_LIMIT = (() => {
  const memory = (navigator as Navigator & { deviceMemory?: number }).deviceMemory;
  const coarse = typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
  return coarse || (memory !== undefined && memory <= 4) ? 256 * 1024 ** 2 : 1024 ** 3;
})();
/** Merge chunks into a Blob every time this much is received (the browser can move large Blobs to disk), to avoid keeping both the chunks and the full file */
const FLUSH_BYTES = 64 * 1024 ** 2;

/** Reason shown when the pre-check fails (a HEAD response has no body, so only the status code can be used) */
/** Message for a failed HEAD/GET, translated when shown (this module may be evaluated before the dictionary is ready) */
function headError(status: number): string {
  switch (status) {
    case 403:
      return t("You don't have permission to download this item");
    case 404:
      return t("Item not found. It may have been deleted or moved.");
    case 503:
      return t("The storage service is temporarily unavailable (no response or the connection failed). Try again later.");
    default:
      return t("Download failed ({status})", { status });
  }
}

let tasks: DownloadTask[] = [];
const listeners = new Set<() => void>();
let seq = 0;

function emit() {
  tasks = [...tasks];
  listeners.forEach((l) => l());
}

function update(id: string, patch: Partial<DownloadTask>) {
  tasks = tasks.map((x) => (x.id === id ? { ...x, ...patch } : x));
  listeners.forEach((l) => l());
}

export function useDownloads() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => tasks,
  );
}

/** Download directly through the browser (large files, public share links) */
export function nativeDownload(url: string) {
  const a = document.createElement("a");
  a.href = url;
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

function filenameFrom(res: Response, fallback: string) {
  const cd = res.headers.get("content-disposition") ?? "";
  const star = /filename\*\s*=\s*UTF-8''([^;]+)/i.exec(cd);
  if (star) {
    try {
      return decodeURIComponent(star[1].trim());
    } catch {
      // If malformed, use the one below
    }
  }
  return /filename\s*=\s*"?([^";]+)"?/i.exec(cd)?.[1] ?? fallback;
}

async function errorOf(res: Response) {
  try {
    const error = ((await res.json()) as { error?: string }).error;
    return error ? tServer(error) : t("Download failed ({status})", { status: res.status });
  } catch {
    return headError(res.status);
  }
}

function save(blob: Blob, name: string) {
  const href = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = href;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // Give the browser time to start saving before releasing
  setTimeout(() => URL.revokeObjectURL(href), 60_000);
}

/** Download with progress; zip means a zipped download (multiple items or folders) */
export async function download(url: string, opts: { zip?: boolean; name?: string } = {}) {
  // First check that it can be downloaded and get the size: show the reason on failure; hand oversized files to the browser
  let size: number | null = null;
  try {
    const head = await fetch(url, { method: "HEAD", credentials: "same-origin" });
    if (!head.ok) {
      // Direct fetch, so the session check in api.request doesn't apply: hand it over the same way
      if (head.status === 401) window.dispatchEvent(new Event("tf:unauthorized"));
      else toast.error(headError(head.status));
      return;
    }
    size = Number(head.headers.get("content-length")) || null;
  } catch {
    // On network errors, try anyway
  }
  if ((size !== null && size > IN_APP_LIMIT) || typeof ReadableStream === "undefined") {
    nativeDownload(url);
    toast.info(t("Large file: downloading directly in your browser. Check the browser's download list for progress."));
    return;
  }

  const id = `d${++seq}`;
  const controller = new AbortController();
  const zip = !!opts.zip;
  tasks = [
    { id, name: opts.name ?? (zip ? t("Download.zip") : t("Downloading…")), zip, total: size, received: 0, rate: 0, status: "downloading", url, controller },
    ...tasks,
  ];
  emit();
  try {
    const res = await fetch(url, { credentials: "same-origin", signal: controller.signal });
    if (res.status === 401) window.dispatchEvent(new Event("tf:unauthorized"));
    if (!res.ok || !res.body) throw new Error(await errorOf(res));
    const name = filenameFrom(res, opts.name ?? "download");
    const total = Number(res.headers.get("content-length")) || size;
    // The server zips multiple items or folders
    update(id, { name, total, zip: zip || res.headers.get("content-type") === "application/zip" });

    const reader = res.body.getReader();
    const parts: Blob[] = [];
    let chunks: Uint8Array[] = [];
    let pending = 0;
    let received = 0;
    let lastAt = performance.now();
    let lastBytes = 0;
    let rate = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      received += value.length;
      pending += value.length;
      if (pending >= FLUSH_BYTES) {
        parts.push(new Blob(chunks as BlobPart[]));
        chunks = [];
        pending = 0;
      }
      // Size unknown up front (e.g. zipped download) and over the limit: let the browser download it directly instead
      if (!total && received > IN_APP_LIMIT) {
        controller.abort();
        tasks = tasks.filter((x) => x.id !== id);
        emit();
        nativeDownload(url);
        toast.info(t("Large file: downloading directly in your browser. Check the browser's download list for progress."));
        return;
      }
      const now = performance.now();
      // Update the display every 0.25 seconds; speed is a smoothed average over recent samples
      if (now - lastAt >= 250) {
        const instant = ((received - lastBytes) * 1000) / (now - lastAt);
        rate = rate ? rate * 0.7 + instant * 0.3 : instant;
        lastAt = now;
        lastBytes = received;
        update(id, { received, rate });
      }
    }
    if (total && received !== total) throw new Error(t("Download incomplete. Please try again."));
    update(id, { received, status: "done" });
    save(new Blob([...parts, ...(chunks as BlobPart[])], { type: res.headers.get("content-type") ?? "application/octet-stream" }), name);
  } catch (e) {
    if (controller.signal.aborted) {
      update(id, { status: "canceled" });
    } else {
      // If zipping fails midway, the server just drops the connection
      const msg = e instanceof TypeError ? t("Download interrupted (network or storage connection lost)") : e instanceof Error ? e.message : t("Download failed");
      update(id, { status: "error", error: msg });
    }
  }
}

export function cancelDownload(id: string) {
  tasks.find((x) => x.id === id)?.controller.abort();
}

export function retryDownload(id: string) {
  const t = tasks.find((x) => x.id === id);
  if (!t) return;
  tasks = tasks.filter((x) => x.id !== id);
  emit();
  void download(t.url, { zip: t.zip, name: t.name });
}

export function clearDownloads() {
  tasks.forEach((x) => x.status === "downloading" && x.controller.abort());
  tasks = [];
  emit();
}

export function removeDownload(id: string) {
  tasks = tasks.filter((x) => x.id !== id);
  emit();
}
