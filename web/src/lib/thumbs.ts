/**
 * Thumbnails of PDFs and videos, made in the browser: the server has no PDF or video decoder, so the first browser
 * that shows such a file in a list draws one (the first page with pdf.js, a frame of the video) and uploads it to the
 * server's thumbnail cache, where it is kept by content like the pictures' thumbnails and served to everyone after
 * that. Where uploading isn't possible (share links), the thumbnail is only kept in this page.
 */
import type { FileSource, Node } from "@/api";

/** Side of the thumbnails made here (the server scales them to its own size) */
export const THUMB_SIDE = 320;
/** Thumbnails made at the same time: each reads part of a file and decodes it */
const PARALLEL = 2;

interface Job {
  /** Still wanted by a list on screen */
  wanted(): boolean;
  start(): Promise<void>;
  /** Dropped without starting */
  skip(): void;
}
const queue: Job[] = [];
let running = 0;

function pump() {
  while (running < PARALLEL && queue.length) {
    const job = queue.shift()!;
    if (!job.wanted()) {
      job.skip();
      continue;
    }
    running++;
    void job.start().finally(() => {
      running--;
      pump();
    });
  }
}

/** Thumbnails kept here at most; the oldest are forgotten first (and the ones made in this page released) */
const KEEP = 300;

/** Finished thumbnails (the URL to show, or null when none can be made) and those on their way, by thumbnail address */
const done = new Map<string, string | null>();
const pending = new Map<string, { promise: Promise<string | null>; users: number; abort: AbortController }>();

/** The server address of the thumbnail names the file and its version (and the share link it is seen through) */
const keyOf = (n: Node, source: FileSource) => source.thumbUrl(n);

function remember(key: string, url: string | null) {
  done.delete(key);
  done.set(key, url);
  for (const [old, oldUrl] of done) {
    if (done.size <= KEEP) break;
    done.delete(old);
    if (oldUrl?.startsWith("blob:")) URL.revokeObjectURL(oldUrl);
  }
}

/** A thumbnail already known for this file version: its URL, null when none can be made, undefined when not known yet */
export function knownThumb(n: Node, source: FileSource): string | null | undefined {
  const key = keyOf(n, source);
  const url = done.get(key);
  // Used again: kept longer
  if (url !== undefined) remember(key, url);
  return url;
}

/**
 * The URL of a file's thumbnail: the server's, when it has one; otherwise one made here (and uploaded when the source
 * allows it). Resolves to null when none can be made. `release` tells that the caller no longer needs it, so a
 * thumbnail not yet started is skipped (e.g. after scrolling past a folder full of PDFs).
 */
export function browserThumb(n: Node, source: FileSource): { promise: Promise<string | null>; release(): void } {
  const key = keyOf(n, source);
  if (done.has(key)) return { promise: Promise.resolve(done.get(key)!), release: () => {} };
  let entry = pending.get(key);
  if (!entry) {
    const e = { users: 0, promise: null as unknown as Promise<string | null>, abort: new AbortController() };
    e.promise = find(n, source, () => e.users > 0, e.abort.signal)
      .then(
        (url) => {
          remember(key, url);
          return url;
        },
        // Skipped or stopped because no list wanted it any more, or it failed (the server or the file couldn't be
        // read): not remembered, so it is tried again next time
        () => null,
      )
      .finally(() => {
        if (pending.get(key) === e) pending.delete(key);
      });
    pending.set(key, e);
    entry = e;
  }
  entry.users++;
  const e = entry;
  let released = false;
  return {
    promise: e.promise,
    release: () => {
      if (released) return;
      released = true;
      // Nobody waits for it any more: stop reading the file; a list that wants it later starts again
      if (--e.users === 0) {
        e.abort.abort();
        if (pending.get(key) === e) pending.delete(key);
      }
    },
  };
}

async function find(n: Node, source: FileSource, wanted: () => boolean, signal: AbortSignal): Promise<string | null> {
  const url = source.thumbUrl(n);
  const res = await fetch(url, { credentials: "same-origin", signal });
  // None yet (204; 404 from servers before 0.6): the page makes one
  const none = res.status === 204 || res.status === 404;
  if (res.ok && !none) {
    // Read to the end, so the browser keeps it for the image that shows it next
    await res.blob();
    return url;
  }
  // Anything but "no thumbnail yet" (no access, a server error) isn't solved by making one: tried again next time
  if (!none) throw new Error(res.statusText);
  const image = await new Promise<Blob | null>((resolve, reject) => {
    queue.push({
      wanted,
      start: () => draw(n, source, signal).then(resolve, reject),
      skip: () => reject(new Error("skipped")),
    });
    pump();
  });
  if (!image) return null;
  if (source.saveThumb) {
    try {
      await source.saveThumb(n, image);
      // Kept on the server now: its copy is what every list shows from here on
      return url;
    } catch {
      // Show the one made here anyway
    }
  }
  return URL.createObjectURL(image);
}

/** The thumbnail, or null when the file can't give one (not a readable PDF or video); fails when it couldn't be read */
async function draw(n: Node, source: FileSource, signal: AbortSignal): Promise<Blob | null> {
  signal.throwIfAborted();
  const canvas = n.mime === "application/pdf" ? await (await import("./pdfThumb")).pdfFirstPage(source.contentUrl(n), THUMB_SIDE, signal) : await videoFrame(source.contentUrl(n), signal);
  return canvas && new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", 0.85));
}

/** A frame a little way into the video (the very first is often black), drawn at thumbnail size */
function videoFrame(url: string, signal: AbortSignal): Promise<HTMLCanvasElement | null> {
  return new Promise((resolve, reject) => {
    const video = document.createElement("video");
    video.muted = true;
    video.preload = "metadata";
    video.playsInline = true;
    let finished = false;
    const finish = (canvas: HTMLCanvasElement | null, error?: Error) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      video.removeAttribute("src");
      video.load();
      if (error) reject(error);
      else resolve(canvas);
    };
    const onAbort = () => finish(null, new Error("stopped"));
    signal.addEventListener("abort", onAbort);
    const timer = setTimeout(() => finish(null, new Error("timed out")), 20_000);
    // A format the browser can't play has no thumbnail; a network error is tried again next time
    video.addEventListener("error", () => {
      const code = video.error?.code;
      const unplayable = code === MediaError.MEDIA_ERR_DECODE || code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED;
      finish(null, unplayable ? undefined : new Error("unreadable"));
    });
    video.addEventListener("loadedmetadata", () => {
      const d = video.duration;
      video.currentTime = Number.isFinite(d) && d > 0 ? Math.min(1, d / 10) : 0;
    });
    video.addEventListener("seeked", () => {
      const { videoWidth: w, videoHeight: h } = video;
      if (!w || !h) return finish(null);
      const scale = Math.min(1, THUMB_SIDE / Math.max(w, h));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(w * scale));
      canvas.height = Math.max(1, Math.round(h * scale));
      canvas.getContext("2d")?.drawImage(video, 0, 0, canvas.width, canvas.height);
      finish(canvas);
    });
    video.src = url;
  });
}
