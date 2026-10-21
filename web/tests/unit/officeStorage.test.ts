import { afterEach, describe, expect, test, vi } from "vitest";
import { OfficeStorage } from "@/components/office/storage";
import { privateSource, type Node } from "@/api";

const node = { id: "office-file", name: "Report.docx", updated_at: 7, size: 4 } as Node;
const bytes = new Uint8Array([0x50, 0x4b, 0x03, 0x04]);
afterEach(() => vi.unstubAllGlobals());

describe("ThirtyFile Office storage", () => {
  test("uses the revision of the actual content, not an older metadata response, and forwards cancellation", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(new Response(bytes, { headers: { "X-Version": "9", ETag: '"content-hash"' } }));
    vi.stubGlobal("fetch", fetch);
    const storage = new OfficeStorage(node, privateSource, true, true);
    const controller = new AbortController();
    const loaded = await storage.load({ id: node.id }, { signal: controller.signal });
    expect(JSON.parse(loaded.revision)).toEqual({ version: 9, etag: '"content-hash"' });
    expect(loaded.content).toEqual(bytes);
    expect(fetch).toHaveBeenCalledWith("/api/files/office-file/content", expect.objectContaining({ signal: controller.signal, cache: "no-store", credentials: "same-origin" }));
  });
  test("sends the SDK's idempotency key and committed revision on consecutive saves", async () => {
    const fetch = vi
      .fn<typeof globalThis.fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...node, updated_at: 10 })))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ...node, updated_at: 11 })));
    vi.stubGlobal("fetch", fetch);
    const storage = new OfficeStorage(node, privateSource, true, true);
    const saved = vi.fn<(node: Node) => void>();
    storage.onSaved = saved;
    const request = {
      content: bytes,
      name: node.name,
      format: "docx" as const,
      baseRevision: JSON.stringify({ version: 9, etag: '"content-hash"' }),
      idempotencyKey: "save-1",
      snapshotId: "state-1",
    };
    const receipt = await storage.save({ id: node.id }, request);
    expect(fetch.mock.calls[0][1]?.headers).toMatchObject({ "X-Base-Version": "9", "If-Match": '"content-hash"', "Idempotency-Key": "save-1" });
    await storage.save({ id: node.id }, { ...request, baseRevision: receipt.revision, idempotencyKey: "save-2" });
    expect(fetch.mock.calls[1][1]?.headers).toMatchObject({ "X-Base-Version": "10", "Idempotency-Key": "save-2" });
    expect(fetch.mock.calls[1][1]?.headers).not.toHaveProperty("If-Match");
    expect(saved).toHaveBeenCalledTimes(2);
  });
  test("distinguishes definite conflicts and permission failures from uncertain server outcomes", async () => {
    for (const [status, code] of [
      [409, "STORAGE_CONFLICT"],
      [403, "STORAGE_FORBIDDEN"],
      [413, "STORAGE_REJECTED"],
    ] as const) {
      vi.stubGlobal("fetch", vi.fn<typeof globalThis.fetch>().mockResolvedValue(new Response(JSON.stringify({ error: "Request refused" }), { status })));
      const storage = new OfficeStorage(node, privateSource, true, true);
      await expect(
        storage.save({ id: node.id }, { content: bytes, name: node.name, format: "docx", baseRevision: '{"version":7,"etag":""}', idempotencyKey: "request", snapshotId: "state" }),
      ).rejects.toMatchObject({ code });
    }
  });
  test("readonly sources cannot save or export when the host forbids it", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(new Response(bytes));
    vi.stubGlobal("fetch", fetch);
    const storage = new OfficeStorage(node, privateSource, false, false);
    expect((await storage.load({ id: node.id })).capabilities).toEqual({ edit: false, export: false });
    await expect(
      storage.save({ id: node.id }, { content: bytes, name: node.name, format: "docx", baseRevision: "unused", idempotencyKey: "request", snapshotId: "state" }),
    ).rejects.toMatchObject({ code: "STORAGE_FORBIDDEN" });
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
