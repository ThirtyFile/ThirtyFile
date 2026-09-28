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

/** Finished thumbnails (the URL to show, or null when none could be made) and those on their way, by thumbnail address */
const done = new Map<string, string | null>();
const pending = new Map<string, { promise: Promise<string | null>; users: number }>();

/** The server address of the thumbnail names the file and its version (and the share link it is seen through) */
const keyOf = (n: Node, source: FileSource) => source.thumbUrl(n);

/** A thumbnail already known for this file version: its URL, null when none can be made, undefined when not known yet */
export function knownThumb(n: Node, source: FileSource): string | null | undefined {
  return done.get(keyOf(n, source));
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
    const e = { users: 0, promise: null as unknown as Promise<string | null> };
    e.promise = find(n, source, () => e.users > 0).then(
      (url) => {
        done.set(key, url);
        return url;
      },
      // Skipped because no list wanted it any more (or the server couldn't be reached): tried again next time
      () => null,
    ).finally(() => pending.delete(key));
    pending.set(key, e);
    entry = e;
  }
  entry.users++;
  const e = entry;
  let released = false;
  return {
    promise: e.promise,
    release: () => {
      if (!released) e.users--;
      released = true;
    },
  };
}

async function find(n: Node, source: FileSource, wanted: () => boolean): Promise<string | null> {
  const url = source.thumbUrl(n);
  const res = await fetch(url, { credentials: "same-origin" });
  if (res.ok) {
    // Read to the end, so the browser keeps it for the image that shows it next
    await res.blob();
    return url;
  }
  // Anything but "no thumbnail yet" (no access, a server error) isn't solved by making one: tried again next time
  if (res.status !== 404) throw new Error(res.statusText);
  const image = await new Promise<Blob | null>((resolve, reject) => {
    queue.push({
      wanted,
      start: () => draw(n, source).then(resolve, () => resolve(null)),
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

async function draw(n: Node, source: FileSource): Promise<Blob | null> {
  const canvas = n.mime === "application/pdf" ? await (await import("./pdfThumb")).pdfFirstPage(source.contentUrl(n), THUMB_SIDE) : await videoFrame(source.contentUrl(n));
  return canvas && new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", 0.85));
}

/** A frame a little way into the video (the very first is often black), drawn at thumbnail size */
function videoFrame(url: string): Promise<HTMLCanvasElement | null> {
  return new Promise((resolve) => {
    const video = document.createElement("video");
    video.muted = true;
    video.preload = "metadata";
    video.playsInline = true;
    let finished = false;
    const finish = (canvas: HTMLCanvasElement | null) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      video.removeAttribute("src");
      video.load();
      resolve(canvas);
    };
    const timer = setTimeout(() => finish(null), 20_000);
    video.addEventListener("error", () => finish(null));
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
