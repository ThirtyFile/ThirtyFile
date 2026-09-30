/**
 * Reports errors people run into to the server's error log (Control panel > Activity > Errors): rendering errors,
 * uncaught errors, rejected promises, and failures the page showed during uploads, file operations and previews.
 *
 * Best effort, and never in the way: reports are sent in the background, never retried in a loop, and a report that
 * can't be sent is dropped (a few wait while the browser is offline). The same error again within a minute isn't sent
 * again, and at most a few reports go out per minute. Nothing here reports its own failures.
 */
import { ApiError } from "@/api";
import { isChunkLoadError } from "@/lib/reload";

export type ErrorKind = "render" | "uncaught" | "rejection" | "handled";

export interface ErrorReport {
  kind: ErrorKind;
  /** What was being done: upload, preview, rename… */
  operation?: string;
  message: string;
  stack?: string;
  route?: string;
  /** The failed request's X-Request-Id, which ties the report to the server's record of it */
  request_id?: string;
  status?: number;
  code?: string;
  /** The id of the item concerned (never its name) */
  resource?: string;
  build?: string;
}

/** Reports per minute */
const PER_MINUTE = 10;
/** The same error again within this time isn't reported again */
const REPEAT_MS = 60_000;
/** Reports kept while offline, sent when the connection is back */
const OFFLINE_QUEUE = 5;
const MAX_MESSAGE = 500;
const MAX_STACK = 4000;

let sentAt: number[] = [];
const recent = new Map<string, number>();
let offline: ErrorReport[] = [];

/** Which build of the page this is: the name of the page's main script (it carries the build's content hash) */
export const BUILD = (() => {
  try {
    const main = typeof document === "undefined" ? null : document.querySelector<HTMLScriptElement>('script[type="module"][src]');
    return new URL(main?.src || import.meta.url).pathname.split("/").pop()?.slice(0, 60) ?? "";
  } catch {
    return "";
  }
})();

/** The page's path without its query (which can hold search terms) and without a share link's token */
export function pageRoute(path: string): string {
  return path.split(/[?#]/)[0].replace(/\/(share|shares)\/[^/]+/, "/$1/…");
}

const clip = (s: string, max: number) => (s.length > max ? `${s.slice(0, max)}…` : s);

/** Builds the report of an error: its message and stack, and for a failed request, its status and request id */
export function describe(kind: ErrorKind, error: unknown, operation?: string, resource?: string): ErrorReport {
  const e = error instanceof Error ? error : null;
  const message = e ? e.message || e.name : typeof error === "string" ? error : String(error);
  const report: ErrorReport = {
    kind,
    operation,
    message: clip(message, MAX_MESSAGE),
    stack: e?.stack ? clip(e.stack, MAX_STACK) : undefined,
    route: pageRoute(location.pathname),
    resource,
    build: BUILD,
  };
  if (error instanceof ApiError) {
    report.status = error.status || undefined;
    report.code = error.code;
    report.request_id = error.requestId;
  }
  return report;
}

/** Whether an error is worth reporting: not a lapsed session, a cancelled request or a site update being loaded */
export function worthReporting(error: unknown): boolean {
  if (error instanceof ApiError && error.status === 401) return false;
  if (error instanceof DOMException && error.name === "AbortError") return false;
  if (isChunkLoadError(error)) return false;
  return true;
}

/**
 * Sends a report. Resolves to whether the server accepted it: false when it was left out (a repeat, over the limit),
 * couldn't be sent, or was refused. Never throws.
 */
export async function report(r: ErrorReport): Promise<boolean> {
  const now = Date.now();
  const key = `${r.kind}|${r.operation ?? ""}|${r.status ?? ""}|${r.message}`;
  const last = recent.get(key);
  if (last !== undefined && now - last < REPEAT_MS) return false;
  sentAt = sentAt.filter((t) => now - t < 60_000);
  if (sentAt.length >= PER_MINUTE) return false;
  recent.set(key, now);
  if (recent.size > 200) for (const [k, t] of recent) if (now - t >= REPEAT_MS) recent.delete(k);
  if (typeof navigator !== "undefined" && navigator.onLine === false) {
    if (offline.length < OFFLINE_QUEUE) offline.push(r);
    return false;
  }
  return send(r);
}

async function send(r: ErrorReport): Promise<boolean> {
  sentAt.push(Date.now());
  try {
    const res = await fetch("/api/client-errors", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(r),
      // Still sent when the page is being closed
      keepalive: true,
    });
    return res.status === 202;
  } catch {
    return false;
  }
}

/** A failure the page showed the person: reported in the background (repeats and lapsed sessions left out) */
export function reportShown(operation: string, error: unknown, resource?: string) {
  if (!worthReporting(error)) return;
  void report(describe("handled", error, operation, resource));
}

/** Rendering errors caught by an error boundary */
export function reportRender(error: unknown, componentStack?: string | null) {
  if (!worthReporting(error)) return;
  const r = describe("render", error);
  if (componentStack) r.stack = clip(`${r.stack ?? ""}\n${componentStack}`.trim(), MAX_STACK);
  void report(r);
}

let installed = false;

/** Reports uncaught errors and rejected promises of this page, and sends reports kept while offline once back */
export function installErrorReporting() {
  if (installed) return;
  installed = true;
  window.addEventListener("error", (e) => {
    // Errors of scripts from elsewhere (browser extensions) and of resources (a missing picture) aren't the page's
    if (e.filename && !e.filename.startsWith(location.origin)) return;
    if (!e.error && !e.message) return;
    if (/ResizeObserver loop/.test(e.message)) return;
    if (!worthReporting(e.error)) return;
    void report(describe("uncaught", e.error ?? e.message));
  });
  window.addEventListener("unhandledrejection", (e) => {
    if (!worthReporting(e.reason)) return;
    void report(describe("rejection", e.reason));
  });
  window.addEventListener("online", () => {
    const waiting = offline;
    offline = [];
    for (const r of waiting) void send(r);
  });
}

/** For tests: forget what was sent */
export function resetErrorReporting() {
  sentAt = [];
  recent.clear();
  offline = [];
}
