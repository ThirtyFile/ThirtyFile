// Thumbnails of PDFs and videos (lib/thumbs.ts): the server's when it has one, asked for once, and remembered
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { FileSource, Node } from "@/api";
import { browserThumb, knownThumb } from "@/lib/thumbs";

let seq = 0;
const pdf = (): Node => ({
  id: `pdf-${++seq}`,
  parent_id: "folder",
  kind: "file",
  name: "report.pdf",
  size: 1000,
  mime: "application/pdf",
  created_at: 0,
  updated_at: seq,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "admin",
  is_favorite: false,
});
const source: FileSource = {
  contentUrl: (n) => `/content/${n.id}`,
  thumbUrl: (n) => `/thumb/${n.id}`,
  downloadLink: () => Promise.resolve(""),
};

const fetch = vi.fn<(url: string, init?: RequestInit) => Promise<Response>>();
beforeEach(() => {
  fetch.mockReset();
  vi.stubGlobal("fetch", fetch);
});
afterEach(() => vi.unstubAllGlobals());

describe("browser-made thumbnails", () => {
  test("the server's thumbnail is used and remembered, and asked for once by lists showing it at the same time", async () => {
    const n = pdf();
    fetch.mockResolvedValue(new Response("jpeg"));
    expect(knownThumb(n, source)).toBeUndefined();
    const a = browserThumb(n, source);
    const b = browserThumb(n, source);
    expect(await a.promise).toBe(`/thumb/${n.id}`);
    expect(await b.promise).toBe(`/thumb/${n.id}`);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(knownThumb(n, source)).toBe(`/thumb/${n.id}`);
    // Known now: not asked for again
    expect(await browserThumb(n, source).promise).toBe(`/thumb/${n.id}`);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  test("a refusal (no access, a server error) shows no thumbnail, and is asked again next time", async () => {
    const n = pdf();
    fetch.mockResolvedValue(new Response("", { status: 403, statusText: "Forbidden" }));
    expect(await browserThumb(n, source).promise).toBeNull();
    expect(knownThumb(n, source)).toBeUndefined();
    await browserThumb(n, source).promise;
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  test("once no list needs it, the request stops", async () => {
    const n = pdf();
    let signal: AbortSignal | undefined;
    fetch.mockImplementation((_, init) => {
      signal = init?.signal ?? undefined;
      return new Promise((_, reject) => signal!.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError"))));
    });
    const a = browserThumb(n, source);
    const b = browserThumb(n, source);
    a.release();
    expect(signal?.aborted).toBe(false);
    b.release();
    expect(signal?.aborted).toBe(true);
    expect(await a.promise).toBeNull();
  });
});
