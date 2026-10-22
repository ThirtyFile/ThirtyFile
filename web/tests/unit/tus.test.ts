// The tus client (lib/tus.ts), against a server that is stood in for: creation, chunks, resuming, retries, termination
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { TusError, TusUpload, encodeMetadata, type TusOptions, type TusResponse } from "@/lib/tus";
import { sessionsOf } from "@/lib/uploadRecovery";

interface Answer {
  status: number;
  headers?: Record<string, string | undefined>;
  body?: string;
}
type Handler = (request: Sent) => Answer | "unreachable" | "hold";
interface Sent {
  method: string;
  url: string;
  headers: Record<string, string>;
  body: Blob | null;
}

/** An XMLHttpRequest that asks `handler` for the answer to each request */
const server = vi.hoisted(() => ({ handler: null as null | Handler, sent: [] as unknown[], progress: 0, aborted: 0 }));
class FakeXhr {
  upload: { onprogress: ((e: { loaded: number }) => void) | null } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  status = 0;
  responseText = "";
  private sent: Sent = { method: "", url: "", headers: {}, body: null };
  private answer: Answer = { status: 0 };
  private held = false;
  open(method: string, url: string) {
    this.sent.method = method;
    this.sent.url = url;
  }
  setRequestHeader(name: string, value: string) {
    this.sent.headers[name] = value;
  }
  getResponseHeader(name: string) {
    const found = Object.entries(this.answer.headers ?? {}).find(([k]) => k.toLowerCase() === name.toLowerCase());
    return found?.[1] ?? null;
  }
  send(body: Blob | null) {
    this.sent.body = body;
    server.sent.push(this.sent);
    queueMicrotask(() => {
      if (this.held) return;
      const result = server.handler!(this.sent);
      if (result === "hold") {
        this.held = true;
        return;
      }
      if (result === "unreachable") return this.onerror?.();
      if (body) this.upload.onprogress?.({ loaded: body.size });
      this.answer = result;
      this.status = result.status;
      this.responseText = result.body ?? "";
      this.onload?.();
    });
  }
  abort() {
    server.aborted++;
    this.held = true;
    this.onabort?.();
  }
}

const requests = () => server.sent as Sent[];
const text = (b: Blob | null) => b?.text() ?? Promise.resolve("");

function options(extra: Partial<TusOptions> = {}) {
  const events = { progress: [] as number[], success: [] as TusResponse[], errors: [] as TusError[] };
  const o: TusOptions = {
    endpoint: "/api/uploads",
    chunkSize: 10,
    retryDelays: [100, 200],
    metadata: { filename: "report.txt", parentId: "folder1" },
    fingerprint: "tf|abc|r1",
    onProgress: (sent) => events.progress.push(sent),
    onSuccess: ({ lastResponse }) => events.success.push(lastResponse),
    onError: (e) => events.errors.push(e),
    onShouldRetry: (e) => (e.response?.status ?? 0) === 0 || e.response!.status >= 500,
    ...extra,
  };
  return { o, events };
}

const file = (content: string) => new File([content], "report.txt");
const base64 = (text: string) => btoa(String.fromCharCode(...new TextEncoder().encode(text)));
const stored = () => Object.keys(localStorage).filter((k) => k.startsWith("tus::"));

/** A server that holds one upload: the creation, the chunks it is sent and what it has */
function uploadServer(size: number, finishHeaders: Record<string, string> = { "X-Node-Id": "n1", "X-Node-Name": "report.txt" }) {
  let have = 0;
  const handler: Handler = (r) => {
    if (r.method === "POST") return { status: 201, headers: { Location: "/api/uploads/u1" } };
    if (r.method === "HEAD") return { status: 200, headers: { "Upload-Offset": String(have), "Upload-Length": String(size) } };
    if (r.method === "PATCH") {
      // A real server refuses a chunk that doesn't start where it is
      if (Number(r.headers["Upload-Offset"]) !== have) return { status: 409 };
      have += r.body!.size;
      return { status: 204, headers: { "Upload-Offset": String(have), ...(have === size ? finishHeaders : {}) } };
    }
    return { status: 204 };
  };
  return { handler, have: () => have };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  server.sent = [];
  server.aborted = 0;
  localStorage.clear();
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const settle = () => vi.advanceTimersByTimeAsync(0);

describe("an upload", () => {
  test("is created with its length and metadata, sent in chunks with progress, and finishes with the last answer", async () => {
    const content = "0123456789abcdefghij12345"; // 25 bytes: chunks of 10, 10 and 5
    server.handler = uploadServer(content.length).handler;
    const { o, events } = options({ metadata: { filename: "Salaries é.txt", parentId: "folder1" } });
    new TusUpload(file(content), o).start();
    await settle();
    await settle();
    await settle();
    await settle();

    const [create, ...chunks] = requests();
    expect(create).toMatchObject({ method: "POST", url: "/api/uploads" });
    expect(create.headers).toMatchObject({ "Tus-Resumable": "1.0.0", "Upload-Length": "25" });
    // The names are UTF-8, in base64
    expect(create.headers["Upload-Metadata"]).toBe(`filename ${base64("Salaries é.txt")},parentId ${base64("folder1")}`);
    expect(chunks.map((c) => [c.method, c.url, c.headers["Upload-Offset"], c.headers["Content-Type"]])).toEqual([
      ["PATCH", "http://localhost:3000/api/uploads/u1", "0", "application/offset+octet-stream"],
      ["PATCH", "http://localhost:3000/api/uploads/u1", "10", "application/offset+octet-stream"],
      ["PATCH", "http://localhost:3000/api/uploads/u1", "20", "application/offset+octet-stream"],
    ]);
    expect(await Promise.all(chunks.map((c) => text(c.body)))).toEqual(["0123456789", "abcdefghij", "12345"]);
    expect(events.progress.at(-1)).toBe(25);
    expect(events.progress).toEqual([...events.progress].sort((a, b) => a - b));
    expect(events.success).toHaveLength(1);
    expect(events.success[0].header("x-node-name")).toBe("report.txt");
    expect(events.errors).toEqual([]);
  });

  test("keeps the server's address while it is being sent, in the records the recovery reads, and forgets it when done", async () => {
    const content = "0123456789abcdefghij";
    const s = uploadServer(content.length);
    let seen: { key: string; url: string }[] = [];
    server.handler = (r) => {
      if (r.method === "PATCH") seen = sessionsOf("tf|abc|r1");
      return s.handler(r);
    };
    const { o, events } = options();
    new TusUpload(file(content), o).start();
    await vi.runAllTimersAsync();
    expect(seen).toHaveLength(1);
    expect(seen[0].url).toBe("http://localhost:3000/api/uploads/u1");
    expect(seen[0].key).toMatch(/^tus::tf\|abc\|r1::\d+$/);
    expect(events.success).toHaveLength(1);
    expect(stored()).toEqual([]);
  });

  test("with no content is created and done, with the answer to its creation", async () => {
    server.handler = (r) => (r.method === "POST" ? { status: 201, headers: { Location: "/api/uploads/u2", "X-Node-Id": "n2" } } : { status: 500 });
    const { o, events } = options();
    new TusUpload(file(""), o).start();
    await vi.runAllTimersAsync();
    expect(requests().map((r) => r.method)).toEqual(["POST"]);
    expect(events.success[0].header("X-Node-Id")).toBe("n2");
    expect(stored()).toEqual([]);
  });

  test("whose creation is refused, or doesn't say where the upload is, fails with what the server said", async () => {
    server.handler = () => ({ status: 507, body: '{"error":"Not enough space"}' });
    let { o, events } = options({ onShouldRetry: () => false });
    new TusUpload(file("abc"), o).start();
    await vi.runAllTimersAsync();
    expect(events.errors[0].response).toMatchObject({ status: 507, body: '{"error":"Not enough space"}' });

    server.handler = () => ({ status: 201 });
    ({ o, events } = options({ onShouldRetry: () => false }));
    new TusUpload(file("abc"), o).start();
    await vi.runAllTimersAsync();
    expect(events.errors[0].message).toMatch(/where the upload is/);
  });

  test("whose chunk is answered with an offset that makes no sense fails", async () => {
    server.handler = (r) => (r.method === "POST" ? { status: 201, headers: { Location: "/u" } } : { status: 204, headers: { "Upload-Offset": "999" } });
    const { o, events } = options({ onShouldRetry: () => false });
    new TusUpload(file("0123456789abc"), o).start();
    await vi.runAllTimersAsync();
    // Stopped at once, not after sending from a position past the end of the file
    expect(requests().map((r) => r.method)).toEqual(["POST", "PATCH"]);
    expect(events.errors.map((e) => e.message)).toEqual([expect.stringMatching(/not valid/)]);
    expect(events.success).toEqual([]);
  });
});

describe("resuming", () => {
  test("asks the server how much it has, and sends the rest", async () => {
    const content = "0123456789abcdefghij12345";
    const s = uploadServer(content.length);
    // The server has the first 10 bytes
    s.handler({ method: "PATCH", url: "", headers: { "Upload-Offset": "0" }, body: new Blob([content.slice(0, 10)]) });
    server.sent = [];
    server.handler = s.handler;
    const { o, events } = options();
    const upload = new TusUpload(file(content), o);
    upload.resume("http://localhost:3000/api/uploads/u1");
    upload.start();
    await vi.runAllTimersAsync();
    expect(requests().map((r) => [r.method, r.headers["Upload-Offset"]])).toEqual([
      ["HEAD", undefined],
      ["PATCH", "10"],
      ["PATCH", "20"],
    ]);
    expect(events.success).toHaveLength(1);
  });

  test("of an upload the server already has all of is done, with the answer to the question", async () => {
    server.handler = () => ({ status: 200, headers: { "Upload-Offset": "5", "Upload-Length": "5", "X-Node-Id": "n9", "X-Node-Name": "kept (1).txt" } });
    const { o, events } = options();
    const upload = new TusUpload(file("12345"), o);
    upload.resume("/api/uploads/u9");
    upload.start();
    await vi.runAllTimersAsync();
    expect(requests().map((r) => r.method)).toEqual(["HEAD"]);
    expect(events.success[0].header("x-node-name")).toBe("kept (1).txt");
  });

  test.each([404, 410, 403])("of an upload the server answers %i for starts a new one, and forgets the old address", async (status) => {
    localStorage.setItem("tus::tf|abc|r1::1", JSON.stringify({ uploadUrl: "/api/uploads/old", creationTime: new Date().toString() }));
    const s = uploadServer(5);
    server.handler = (r) => (r.method === "HEAD" ? { status } : s.handler(r));
    const { o, events } = options();
    const upload = new TusUpload(file("12345"), o);
    upload.resume("/api/uploads/old");
    upload.start();
    await vi.runAllTimersAsync();
    expect(requests().map((r) => r.method)).toEqual(["HEAD", "POST", "PATCH"]);
    expect(events.success).toHaveLength(1);
    expect(stored()).toEqual([]);
  });

  test("keeps a record the page can find again in the format the earlier tus-js-client kept", () => {
    localStorage.setItem("tus::tf|abc|r1::111", JSON.stringify({ uploadUrl: "/api/uploads/older", creationTime: "Mon Oct 19 2026 10:00:00 GMT+0000", size: 5, metadata: {} }));
    localStorage.setItem("tus::tf|abc|r1::222", JSON.stringify({ uploadUrl: "/api/uploads/newer", creationTime: "Mon Oct 19 2026 11:00:00 GMT+0000", size: 5, metadata: {} }));
    localStorage.setItem("tus::tf|other|r2::333", JSON.stringify({ uploadUrl: "/api/uploads/x", creationTime: new Date().toString() }));
    expect(sessionsOf("tf|abc|r1").map((s) => s.url)).toEqual(["/api/uploads/newer", "/api/uploads/older"]);
  });
});

describe("retries", () => {
  test("wait the delays, ask where the server is, and go on from there", async () => {
    const content = "0123456789abcdefghij";
    const s = uploadServer(content.length);
    let failures = 1;
    server.handler = (r) => (r.method === "PATCH" && r.headers["Upload-Offset"] === "10" && failures-- > 0 ? { status: 503 } : s.handler(r));
    const { o, events } = options();
    new TusUpload(file(content), o).start();
    await settle();
    await settle();
    await settle();
    // Failed: waiting
    expect(requests().map((r) => r.method)).toEqual(["POST", "PATCH", "PATCH"]);
    await vi.advanceTimersByTimeAsync(99);
    expect(requests()).toHaveLength(3);
    await vi.advanceTimersByTimeAsync(1);
    await vi.runAllTimersAsync();
    expect(requests().map((r) => r.method)).toEqual(["POST", "PATCH", "PATCH", "HEAD", "PATCH"]);
    expect(events.success).toHaveLength(1);
    expect(events.errors).toEqual([]);
  });

  test("end with the error once the delays are used up, and not at all when the caller says no", async () => {
    server.handler = () => ({ status: 503 });
    let { o, events } = options();
    new TusUpload(file("abc"), o).start();
    await vi.runAllTimersAsync();
    // The first try and one for each delay
    expect(requests()).toHaveLength(3);
    expect(events.errors).toHaveLength(1);
    expect(events.errors[0].response?.status).toBe(503);

    server.sent = [];
    server.handler = () => ({ status: 403 });
    ({ o, events } = options());
    new TusUpload(file("abc"), o).start();
    await vi.runAllTimersAsync();
    expect(requests()).toHaveLength(1);
    expect(events.errors[0].response?.status).toBe(403);
  });

  test("count from the start again whenever the server got further", async () => {
    // Every chunk fails once: with two delays a third failure in a row would end it, but each retry made progress
    const content = "0123456789abcdefghij12345678901234";
    const s = uploadServer(content.length);
    const failed = new Set<string>();
    server.handler = (r) => {
      const at = r.headers["Upload-Offset"];
      if (r.method === "PATCH" && !failed.has(at)) {
        failed.add(at);
        return { status: 503 };
      }
      return s.handler(r);
    };
    const { o, events } = options({ retryDelays: [10] });
    new TusUpload(file(content), o).start();
    await vi.runAllTimersAsync();
    expect(events.success).toHaveLength(1);
    expect(events.errors).toEqual([]);
  });

  test("follow a server that couldn't be reached", async () => {
    const s = uploadServer(3);
    let down = 1;
    server.handler = (r) => (down-- > 0 ? "unreachable" : s.handler(r));
    const { o, events } = options();
    new TusUpload(file("abc"), o).start();
    await vi.runAllTimersAsync();
    expect(events.success).toHaveLength(1);
  });
});

describe("stopping", () => {
  test("pausing stops the request and keeps the address for resuming, and reports no error", async () => {
    const s = uploadServer(25);
    server.handler = (r) => (r.method === "PATCH" ? "hold" : s.handler(r));
    const { o, events } = options();
    const upload = new TusUpload(file("0123456789abcdefghij12345"), o);
    upload.start();
    await settle();
    await settle();
    await upload.abort();
    await vi.runAllTimersAsync();
    expect(server.aborted).toBe(1);
    expect(events.errors).toEqual([]);
    expect(events.success).toEqual([]);
    expect(stored()).toHaveLength(1);
    expect(requests().filter((r) => r.method === "DELETE")).toEqual([]);
  });

  test("cancelling ends the upload on the server and forgets the address", async () => {
    const s = uploadServer(25);
    server.handler = (r) => (r.method === "PATCH" ? "hold" : s.handler(r));
    const { o, events } = options();
    const upload = new TusUpload(file("0123456789abcdefghij12345"), o);
    upload.start();
    await settle();
    await settle();
    await upload.abort(true);
    await vi.runAllTimersAsync();
    const sent = requests();
    expect(sent.at(-1)).toMatchObject({ method: "DELETE", url: "http://localhost:3000/api/uploads/u1" });
    expect(stored()).toEqual([]);
    expect(events.errors).toEqual([]);
  });

  test("while waiting to retry cancels the retry", async () => {
    server.handler = () => ({ status: 503 });
    const { o, events } = options();
    const upload = new TusUpload(file("abc"), o);
    upload.start();
    await settle();
    await upload.abort();
    await vi.runAllTimersAsync();
    expect(requests()).toHaveLength(1);
    expect(events.errors).toEqual([]);
  });
});

test("metadata is UTF-8 in base64, and what is undefined is left out", () => {
  expect(encodeMetadata({ filename: "é", empty: "", skipped: undefined as unknown as string })).toBe(`filename ${base64("é")},empty `);
});
