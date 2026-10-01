// What is kept to recover interrupted uploads after a reload (lib/uploadRecovery.ts)
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { UploadRecord } from "@/lib/uploadRecovery";

/** The module as a page loaded afresh in this tab has it */
async function load() {
  vi.resetModules();
  return import("@/lib/uploadRecovery");
}

const DAY = 86_400_000;

function record(over: Partial<UploadRecord> = {}): UploadRecord {
  const now = Date.now();
  return {
    id: "r1",
    name: "Report.pdf",
    relativePath: "",
    parentId: "folder1",
    batch: "b1",
    size: 1000,
    lastModified: 1,
    onConflict: "keep",
    sent: 500,
    state: "sending",
    created: now,
    updated: now,
    tab: "other-tab",
    beat: now,
    ...over,
  };
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

describe("whose records", () => {
  test("a person's own uploads need them to be known; a link's are kept by a hash of its address", async () => {
    const m = await load();
    expect(m.scopeOf("/api/uploads")).toBeNull();
    m.setRecoveryUser(7);
    expect(m.scopeOf("/api/uploads")).toBe("u7");
    const scope = m.scopeOf("/api/public/shares/SeCrEtToKeN/uploads")!;
    expect(scope).toMatch(/^s[0-9a-f]{16}$/);
    expect(scope).not.toContain("SeCrEtToKeN");
    expect(m.scopeOf("/api/public/shares/Other/uploads")).not.toBe(scope);
  });

  test("records hold no file content and nothing that signs in", async () => {
    const m = await load();
    m.saveLive("u7", [record({ tab: m.TAB })]);
    const kept = localStorage.getItem("tf-upload-tasks-u7")!;
    expect(Object.keys(JSON.parse(kept)[0]).sort()).toEqual(
      ["batch", "beat", "created", "id", "lastModified", "name", "onConflict", "parentId", "relativePath", "sent", "size", "state", "tab", "updated"].sort(),
    );
  });
});

describe("which uploads were interrupted", () => {
  test("another open tab's uploads aren't, until that tab stops saying so", async () => {
    const m = await load();
    const now = Date.now();
    localStorage.setItem("tf-upload-tasks-u7", JSON.stringify([record({ id: "fresh", beat: now }), record({ id: "stale", beat: now - m.STALE_MS - 1000 })]));
    expect(m.interrupted("u7", new Set(), now).map((r) => r.id)).toEqual(["stale"]);
  });

  test("this tab's uploads from before a reload are, at once", async () => {
    sessionStorage.setItem("tf-upload-tabs", JSON.stringify(["before-reload"]));
    const m = await load();
    localStorage.setItem("tf-upload-tasks-u7", JSON.stringify([record({ tab: "before-reload", beat: Date.now() })]));
    expect(m.interrupted("u7", new Set()).map((r) => r.id)).toEqual(["r1"]);
    // Continuing one in this tab takes it off the list
    expect(m.interrupted("u7", new Set(["r1"]))).toEqual([]);
  });

  test("saving this tab's uploads keeps other tabs' and waiting ones, and drops this tab's finished ones", async () => {
    const m = await load();
    localStorage.setItem("tf-upload-tasks-u7", JSON.stringify([record({ id: "other" }), record({ id: "mine-finished", tab: m.TAB }), record({ id: "mine", tab: m.TAB, sent: 1 })]));
    m.saveLive("u7", [record({ id: "mine", tab: m.TAB, sent: 900 })]);
    const kept: UploadRecord[] = JSON.parse(localStorage.getItem("tf-upload-tasks-u7")!);
    expect(kept.map((r) => [r.id, r.sent])).toEqual([
      ["other", 500],
      ["mine", 900],
    ]);
    // Nothing left: the key goes
    m.removeRecords("u7", ["other"]);
    m.saveLive("u7", []);
    expect(localStorage.getItem("tf-upload-tasks-u7")).toBeNull();
  });

  test("records expire with the server's uploads, and there is a limit to them", async () => {
    const m = await load();
    localStorage.setItem("tf-upload-tasks-u7", JSON.stringify([record({ id: "old", updated: Date.now() - 8 * DAY }), record({ id: "new" }), { junk: true }]));
    expect(m.readRecords("u7").map((r) => r.id)).toEqual(["new"]);
    const many = Array.from({ length: m.MAX_RECORDS + 10 }, (_, i) => record({ id: `r${i}`, tab: m.TAB }));
    m.saveLive("u7", many);
    expect(m.readRecords("u7")).toHaveLength(m.MAX_RECORDS);
  });

  test("uploads added together are grouped, with an uploaded folder's name", async () => {
    const m = await load();
    const groups = m.groupByBatch([
      record({ id: "a", batch: "f", relativePath: "Photos/2024", name: "a.jpg", size: 100, sent: 100 }),
      record({ id: "b", batch: "f", relativePath: "Photos", name: "b.jpg", size: 100, sent: 20 }),
      record({ id: "c", batch: "x", name: "c.txt", size: 10, sent: 0 }),
    ]);
    expect(groups.map((g) => [g.batch, g.folder, g.records.length, g.size, g.sent])).toEqual([
      ["f", "Photos", 2, 200, 120],
      ["x", null, 1, 10, 0],
    ]);
  });
});

describe("telling files apart", () => {
  test("a sample of the content tells a replaced file from the original, even with the same size", async () => {
    const m = await load();
    const big = new Uint8Array(1_000_000).map((_, i) => i % 251);
    const same = await m.sampleOf(new Blob([big]));
    expect(await m.sampleOf(new Blob([big.slice()]))).toBe(same);
    // Another file of the same size
    expect(await m.sampleOf(new Blob([big.map((b) => b ^ 0x55)]))).not.toBe(same);
    // Changes at the start, the end, or anywhere in a sampled place
    for (const at of [0, 999_999, 531_200]) {
      const changed = big.slice();
      changed[at] ^= 1;
      expect(await m.sampleOf(new Blob([changed]))).not.toBe(same);
    }
    const small = new TextEncoder().encode("hello");
    expect(await m.sampleOf(new Blob([small]))).not.toBe(await m.sampleOf(new Blob([new TextEncoder().encode("hellO")])));
  });

  test("the tus fingerprint is the one uploads already use, so earlier uploads still continue", async () => {
    const m = await load();
    const fp = m.fingerprintOf("/api/uploads", { parentId: "p", relativePath: "A", onConflict: "keep", name: "f.txt", size: 3, lastModified: 9 });
    expect(fp).toBe(["sd", "/api/uploads", "p", "A", "keep", "f.txt", 3, 9].join("|"));
    localStorage.setItem(`tus::${fp}::1`, JSON.stringify({ uploadUrl: "/api/uploads/abc" }));
    expect(m.sessionsOf(fp)).toEqual([{ key: `tus::${fp}::1`, url: "/api/uploads/abc" }]);
  });
});

describe("clearing", () => {
  test("someone else signing in keeps only their records; signing out keeps none", async () => {
    const m = await load();
    for (const k of ["u1", "u2", "s0123456789abcdef"]) localStorage.setItem(`tf-upload-tasks-${k}`, "[]");
    m.forgetRecords("u1", false);
    expect(Object.keys(localStorage).sort()).toEqual(["tf-upload-tasks-s0123456789abcdef", "tf-upload-tasks-u1"]);
    m.forgetRecords("u1", true);
    expect(Object.keys(localStorage)).toEqual(["tf-upload-tasks-u1"]);
    m.forgetRecords(null, true);
    expect(Object.keys(localStorage)).toEqual([]);
  });
});
