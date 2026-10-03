//! Download manager: downloads in the page with progress (name, percentage, speed, time left), then hands the file to the browser to save.
//! Multi-item downloads are zipped by the server while streaming, with the total size announced up front, so progress can be shown too.
//! Files larger than IN_APP_LIMIT aren't buffered in the page (to avoid using lots of memory); the browser downloads them directly instead.

import { toast } from "sonner";
import { responseError } from "@/api";
import { t } from "@/lib/i18n";
import { createStore, useStore } from "@/lib/store";
import { errorMessage } from "@/lib/utils";

export type DownloadStatus = "preparing" | "downloading" | "done" | "error" | "canceled";

export interface DownloadTask {
  id: string;
  name: string;
  /** Zipped download (multiple items or folders) */
  zip: boolean;
  /** Public downloads stay native so a handoff cannot count the download twice. */
  native?: boolean;
  /** Total size; null when the server doesn't provide it */
  total: number | null;
  received: number;
  /** Recent download speed (bytes/second) */
  rate: number;
  status: DownloadStatus;
  error?: string;
  /** Where the download came from, to start it again */
  source: DownloadSource;
  controller: AbortController;
}

/** A URL, or a function that makes one when the download starts (a short-lived link for several items) */
export type DownloadSource = string | ((signal?: AbortSignal) => Promise<string>);

/**
 * Limit for downloading in the page; larger files are handed to the browser to download directly (to disk, without
 * holding the file in memory). Phones (touch devices) or computers with little memory use a lower limit, so the tab
 * isn't killed by the system for running out of memory. It is also the most that is fetched and thrown away when the
 * size isn't known up front
 */
const IN_APP_LIMIT = (() => {
  const memory = (navigator as Navigator & { deviceMemory?: number }).deviceMemory;
  const coarse = typeof matchMedia !== "undefined" && matchMedia("(pointer: coarse)").matches;
  return coarse || (memory !== undefined && memory <= 4) ? 64 * 1024 ** 2 : 256 * 1024 ** 2;
})();
/** Merge chunks into a Blob every time this much is received (the browser can move large Blobs to disk), to avoid keeping both the chunks and the full file */
const FLUSH_BYTES = 16 * 1024 ** 2;

/** Message for a failed download whose response carries no readable reason, translated when shown (this module may be evaluated before the dictionary is ready) */
function statusError(status: number): string {
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

const tasks = createStore<DownloadTask[]>([]);
let seq = 0;

function update(id: string, patch: Partial<DownloadTask>) {
  tasks.set(tasks.get().map((x) => (x.id === id ? { ...x, ...patch } : x)));
}

export function useDownloads() {
  return useStore(tasks);
}

/** Download directly through the browser (large files, public share links) */
export function nativeDownload(url: string, announce = true) {
  const a = document.createElement("a");
  a.href = url;
  a.download = "";
  document.body.appendChild(a);
  a.click();
  a.remove();
  if (announce) toast.info(t("Downloading directly in your browser. Check the browser's download list for progress."));
}

export function filenameFrom(res: Response, fallback: string) {
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

/** Saves what the page made (a file it generated, or a download it read) under this name */
export function saveBlob(blob: Blob, name: string) {
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
export async function download(source: DownloadSource, opts: { zip?: boolean; name?: string; native?: boolean } = {}) {
  const id = `d${++seq}`;
  const controller = new AbortController();
  const zip = !!opts.zip;
  tasks.set([
    { id, name: opts.name ?? (zip ? t("Download.zip") : t("Downloading…")), zip, native: opts.native, total: null, received: 0, rate: 0, status: "preparing", source, controller },
    ...tasks.get(),
  ]);
  const drop = () => tasks.set(tasks.get().filter((x) => x.id !== id));
  // Too large to keep in the page: stop reading and let the browser download it itself (to disk, with its own
  // progress). A short-lived link may have expired while the first part was read: a new one is asked for first.
  const handOver = async (url: string, fresh: boolean) => {
    const next = fresh && typeof source !== "string" ? await source(controller.signal) : url;
    if (controller.signal.aborted) return;
    controller.abort();
    drop();
    nativeDownload(next, false);
    toast.info(t("Large file: downloading directly in your browser. Check the browser's download list for progress."));
  };
  try {
    const url = typeof source === "string" ? source : await source(controller.signal);
    if (controller.signal.aborted) return;
    if (opts.native || typeof ReadableStream === "undefined") {
      drop();
      nativeDownload(url);
      return;
    }
    // One request: the status and size come with the response, so there is no separate check first (a HEAD request
    // would make the server open the file or walk the folder once more)
    const res = await fetch(url, { credentials: "same-origin", signal: controller.signal });
    if (controller.signal.aborted) return;
    if (!res.ok) {
      const error = await responseError(res, url, statusError(res.status));
      if (res.status === 401) {
        // The session expired: responseError sent the user to sign in, like api.request
        controller.abort();
        drop();
        return;
      }
      throw error;
    }
    if (!res.body) throw new Error(statusError(res.status));
    const total = Number(res.headers.get("content-length")) || null;
    if (total !== null && total > IN_APP_LIMIT) return await handOver(url, false);
    const name = filenameFrom(res, opts.name ?? "download");
    // The server zips multiple items or folders
    update(id, { name, total, status: "downloading", zip: zip || res.headers.get("content-type") === "application/zip" });

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
      if (controller.signal.aborted) return;
      if (done) break;
      chunks.push(value);
      received += value.length;
      pending += value.length;
      if (pending >= FLUSH_BYTES) {
        parts.push(new Blob(chunks as BlobPart[]));
        chunks = [];
        pending = 0;
      }
      // Size unknown up front (rare: e.g. a proxy that compresses responses) and over the limit: let the browser download it directly instead
      if (!total && received > IN_APP_LIMIT) return await handOver(url, true);
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
    if (controller.signal.aborted) return;
    update(id, { received, status: "done" });
    saveBlob(new Blob([...parts, ...(chunks as BlobPart[])], { type: res.headers.get("content-type") ?? "application/octet-stream" }), name);
  } catch (e) {
    if (controller.signal.aborted) {
      update(id, { status: "canceled" });
    } else {
      // If zipping fails midway, the server just drops the connection
      const msg = e instanceof TypeError ? t("Download interrupted (network or storage connection lost)") : errorMessage(e, t("Download failed"));
      update(id, { status: "error", error: msg });
      controller.abort();
    }
  }
}

export function cancelDownload(id: string) {
  tasks
    .get()
    .find((x) => x.id === id)
    ?.controller.abort();
  update(id, { status: "canceled" });
}

export function retryDownload(id: string) {
  const t = tasks.get().find((x) => x.id === id);
  if (!t) return;
  t.controller.abort();
  tasks.set(tasks.get().filter((x) => x.id !== id));
  void download(t.source, { zip: t.zip, name: t.name, native: t.native });
}

export function clearDownloads() {
  tasks.get().forEach((x) => (x.status === "downloading" || x.status === "preparing") && x.controller.abort());
  tasks.set([]);
}

export function removeDownload(id: string) {
  cancelDownload(id);
  tasks.set(tasks.get().filter((x) => x.id !== id));
}

/** Trigger a browser download (without leaving the page) */
/**
 * Downloads: signed-in downloads show progress in the page (bottom right) and are saved when done;
 * public share links are handed to the browser to download directly (handing a large file over to the browser after starting it in the page would count
 * the download twice).
 * A function is called for the URL when the download starts (and again when it is retried)
 */
export function triggerDownload(source: DownloadSource) {
  if (typeof source === "string" && source.startsWith("/api/public/")) return nativeDownload(source);
  return download(source);
}
