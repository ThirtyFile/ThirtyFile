// Reporting errors to the server's error log (lib/errorReport.ts): what a report holds, and that reporting stays
// bounded and out of the way
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ApiError } from "@/api";
import { errorFromBody } from "@/api/client";
import { describe as describeError, pageRoute, report, reportShown, resetErrorReporting, worthReporting, type ErrorReport } from "@/lib/errorReport";

const sent: ErrorReport[] = [];
let answer: () => Promise<Response>;

beforeEach(() => {
  resetErrorReporting();
  sent.length = 0;
  answer = () => Promise.resolve(new Response('{"recorded":true}', { status: 202 }));
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>((_url, init) => {
      sent.push(JSON.parse(String(init?.body)));
      return answer();
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

const one = (message: string): ErrorReport => ({ kind: "uncaught", message });

describe("a report", () => {
  test("of a failed request carries its status, code and request id", () => {
    const r = describeError("handled", new ApiError("No permission", 403, "forbidden", "abc123"), "upload", "node1");
    expect(r).toMatchObject({ kind: "handled", operation: "upload", message: "No permission", status: 403, code: "forbidden", request_id: "abc123", resource: "node1" });
  });

  test("of a failed request has the server's own message, not the one translated for the page", () => {
    // The server's message is kept with the error
    const failed = errorFromBody(409, JSON.stringify({ error: '"Secret plan.docx" already exists' }), "/api/nodes/x/rename", "failed");
    expect(failed.serverMessage).toBe('"Secret plan.docx" already exists');
    // and reported instead of what a Traditional Chinese page shows, whose quotes the server doesn't know
    const shown = new ApiError("「Secret plan.docx」已存在", 409, undefined, undefined, '"Secret plan.docx" already exists');
    const r = describeError("handled", shown, "rename");
    expect(r.message).toBe('"Secret plan.docx" already exists');
    expect(r.stack ?? "").not.toContain("已存在");
  });

  test("leaves the query and a share link's token out of the page's path, and shortens long text", () => {
    expect(pageRoute("/share/SeCrEtToKeN/abc")).toBe("/share/…/abc");
    expect(pageRoute("/search?q=salaries#x")).toBe("/search");
    const e = new Error("x".repeat(2000));
    const r = describeError("render", e);
    expect(r.message.length).toBeLessThanOrEqual(501);
    expect(r.stack!.length).toBeLessThanOrEqual(4001);
  });

  test("isn't made for a lapsed session, a cancelled request or a site update", () => {
    expect(worthReporting(new ApiError("Please sign in", 401))).toBe(false);
    expect(worthReporting(new DOMException("aborted", "AbortError"))).toBe(false);
    expect(worthReporting(new TypeError("Failed to fetch dynamically imported module: /assets/x.js"))).toBe(false);
    expect(worthReporting(new Error("boom"))).toBe(true);
  });
});

describe("reporting", () => {
  test("says whether the server accepted the report", async () => {
    expect(await report(one("a"))).toBe(true);
    answer = () => Promise.resolve(new Response('{"recorded":false}', { status: 429 }));
    expect(await report(one("b"))).toBe(false);
    // A failed request is dropped quietly, never thrown or retried
    answer = () => Promise.reject(new TypeError("Failed to fetch"));
    expect(await report(one("c"))).toBe(false);
    expect(sent.map((r) => r.message)).toEqual(["a", "b", "c"]);
  });

  test("sends the same error once a minute, and at most ten reports a minute", async () => {
    vi.useFakeTimers();
    expect(await report(one("same"))).toBe(true);
    expect(await report(one("same"))).toBe(false);
    for (let i = 0; i < 20; i++) await report(one(`other ${i}`));
    expect(sent).toHaveLength(10);
    vi.advanceTimersByTime(61_000);
    expect(await report(one("same"))).toBe(true);
    expect(sent).toHaveLength(11);
  });

  test("keeps a few reports while offline, without sending them", async () => {
    vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
    for (let i = 0; i < 8; i++) expect(await report(one(`offline ${i}`))).toBe(false);
    expect(sent).toHaveLength(0);
  });

  test("of a failure shown to the person runs in the background", async () => {
    reportShown("preview", new ApiError("A server error occurred", 500, undefined, "r1"), "n1");
    reportShown("preview", new ApiError("Please sign in", 401));
    await vi.waitFor(() => expect(sent).toHaveLength(1));
    expect(sent[0]).toMatchObject({ kind: "handled", operation: "preview", status: 500, request_id: "r1", resource: "n1" });
  });
});
