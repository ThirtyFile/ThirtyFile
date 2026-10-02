// The upload queue (uploads.ts): a few files at a time, pause, resume, retry and cancel, and telling the lists when
// files land. tus-js-client is stood in for: each upload is started, and succeeds or fails when a test says so.
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

interface FakeUpload {
  file: File;
  options: {
    onSuccess(payload: { lastResponse: { getHeader(name: string): string | undefined } }): void;
    onError(err: Error): void;
    onProgress(sent: number, total: number): void;
  };
  started: boolean;
  aborted: boolean;
}
const uploads = vi.hoisted(() => [] as FakeUpload[]);
vi.mock("tus-js-client", () => ({
  Upload: class {
    started = false;
    aborted = false;
    constructor(
      public file: File,
      public options: FakeUpload["options"],
    ) {
      uploads.push(this as unknown as FakeUpload);
    }
    start() {
      this.started = true;
    }
    abort() {
      this.aborted = true;
      return Promise.resolve();
    }
    findPreviousUploads() {
      return Promise.resolve([]);
    }
    resumeFromPreviousUpload() {}
  },
}));

// A failure shown is reported to the server: not from here
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));

// Content identities: counted, and held back while a test says so
const identity = vi.hoisted(() => ({ computed: 0, hold: null as Promise<void> | null }));
vi.mock("@/lib/uploadRecovery", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/lib/uploadRecovery")>();
  return {
    ...real,
    sampleOf: async (file: Blob, signal?: AbortSignal) => {
      identity.computed++;
      if (identity.hold) await identity.hold;
      return real.sampleOf(file, signal);
    },
  };
});

const up = await import("@/uploads");
const recovery = await import("@/lib/uploadRecovery");
const { api } = await import("@/api");
type Snapshot = ReturnType<typeof up.useUploads>;

/** What the upload panel would show */
let shown: Snapshot;
function Probe() {
  shown = up.useUploads();
  return null;
}
const root = createRoot(document.createElement("div"));
beforeAll(() => act(() => root.render(createElement(Probe))));

const files = (n: number, folder = "") => Array.from({ length: n }, (_, i) => ({ file: new File([`content ${i}`], `file${i}.txt`), relativePath: folder }));
const started = () => uploads.filter((u) => u.started && !u.aborted);
const statuses = () => shown.tasks.map((t) => t.status);
const succeed = (u: FakeUpload) => act(() => u.options.onSuccess({ lastResponse: { getHeader: () => undefined } }));
/** Lets the queue start what it can (each start is a few promises), and shows the change */
const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, 300))));

afterEach(async () => {
  act(() => up.cancelAll());
  uploads.length = 0;
  await settle();
});

describe("upload queue", () => {
  test("three files are sent at a time, and the next starts when one ends", async () => {
    act(() => up.enqueue(files(5), "folder"));
    await settle();
    expect(started()).toHaveLength(3);
    expect(shown.totals).toMatchObject({ uploading: 3, queued: 2 });
    expect(up.hasActiveUploads()).toBe(true);

    succeed(started()[0]);
    await settle();
    expect(started()).toHaveLength(4);
    expect(shown.totals).toMatchObject({ done: 1, uploading: 3, queued: 1 });
  });

  test("a paused upload lets the next one start, and continues when resumed", async () => {
    act(() => up.enqueue(files(4), "folder"));
    await settle();
    const first = shown.tasks[0];
    act(() => up.pause(first.id));
    await settle();
    expect(shown.tasks[0].status).toBe("paused");
    expect(started().map((u) => u.file.name)).toEqual(["file1.txt", "file2.txt", "file3.txt"]);

    act(() => up.resume(first.id));
    await settle();
    expect(shown.tasks[0].status).toBe("queued");
    succeed(started()[0]);
    await settle();
    expect(shown.tasks[0].status).toBe("uploading");
  });

  test("a failed upload says why: the server's message, or a lost connection; and Retry sends it again", async () => {
    act(() => up.enqueue(files(2), "folder"));
    await settle();
    const refused = Object.assign(new Error("tus: unexpected response"), {
      originalResponse: { getStatus: () => 507, getBody: () => JSON.stringify({ error: "Not enough space" }), getHeader: () => undefined },
    });
    act(() => started()[0].options.onError(refused));
    act(() => started()[1].options.onError(new Error("tus: failed to upload chunk")));
    await settle();
    expect(shown.tasks.map((t) => [t.status, t.error])).toEqual([
      ["error", "Not enough space"],
      ["error", "Network connection lost"],
    ]);
    expect(up.hasActiveUploads()).toBe(false);

    act(() => up.retryFailed());
    await settle();
    expect(statuses()).toEqual(["uploading", "uploading"]);
    expect(started()).toHaveLength(4);
  });

  test("cancelling one takes it off the list; cancelling all stops every upload", async () => {
    act(() => up.enqueue(files(4), "folder"));
    await settle();
    act(() => up.cancel(shown.tasks[3].id));
    await settle();
    expect(shown.tasks).toHaveLength(3);
    act(() => up.cancelAll());
    await settle();
    expect(shown.tasks).toHaveLength(0);
    expect(uploads.every((u) => !u.started || u.aborted)).toBe(true);
  });

  test("the lists are told where files landed, and once more when nothing is left to send", async () => {
    const landed = vi.fn<(parentIds: string[], final: boolean, batch: { folders: string[]; trees: string[] }) => void>();
    const stop = up.onUploadsLanded(landed);
    act(() => up.enqueue(files(1), "plain"));
    act(() => up.enqueue(files(1, "Photos"), "tree"));
    await settle();
    for (const u of started()) succeed(u);
    await settle();
    expect(landed).toHaveBeenLastCalledWith(["plain"], true, { folders: ["plain"], trees: ["tree"] });
    act(() => up.clearFinished());
    await settle();
    expect(shown.tasks).toHaveLength(0);
    stop();
  });
});

describe("content identity", () => {
  beforeEach(() => void (identity.computed = 0));
  afterEach(() => {
    identity.hold = null;
    recovery.setRecoveryUser(null);
    localStorage.clear();
    vi.restoreAllMocks();
  });
  const kept = () => recovery.readRecords("u7");

  test("a file is sent before its identity is known, which its record gets once computed", async () => {
    recovery.setRecoveryUser(7);
    let release!: () => void;
    identity.hold = new Promise((r) => (release = r));
    act(() => up.enqueue(files(1), "folder"));
    await settle();
    expect(started()).toHaveLength(1);
    expect(identity.computed).toBe(1);
    expect(kept()[0].sample).toBeUndefined();
    release();
    await act(async () => void (await new Promise((r) => setTimeout(r, 1200))));
    expect(kept()[0].sample).toMatch(/^sha256-v1:/);
  });

  test("a file chosen again after a reload is read once: to tell whether it is the same, not again to send it", async () => {
    recovery.setRecoveryUser(7);
    const file = new File(["the same content"], "a.txt", { lastModified: 5 });
    const changed = new File(["other content!!!"], "b.txt", { lastModified: 5 });
    const now = Date.now();
    const base = {
      relativePath: "",
      parentId: "folder",
      batch: "b",
      lastModified: 5,
      onConflict: "keep" as const,
      sent: 4,
      state: "sending" as const,
      created: now,
      updated: now,
      tab: "gone",
      beat: 0,
    };
    const sample = await recovery.sampleOf(file);
    identity.computed = 0;
    const records = [
      { ...base, id: "r1", name: "a.txt", size: file.size, sample },
      { ...base, id: "r2", name: "b.txt", size: changed.size, sample },
    ];
    let result;
    await act(
      async () =>
        void (result = await up.resumeRecovered("/api/uploads", records, [
          { file, relativePath: "" },
          { file: changed, relativePath: "" },
        ])),
    );
    await settle();
    expect(result).toMatchObject({ continuing: 1, changed: 1 });
    expect(started()).toHaveLength(2);
    expect(identity.computed).toBe(2);
  });

  test("more loose files than the server checks at once are checked for name clashes in parts", async () => {
    const conflicts = vi.spyOn(api, "conflicts").mockResolvedValue([]);
    await act(() => up.uploadFiles(files(10_001), "folder"));
    expect(conflicts.mock.calls.map(([req]) => req.names!.length)).toEqual([10_000, 1]);
    expect(shown.tasks).toHaveLength(10_001);
  });
});
