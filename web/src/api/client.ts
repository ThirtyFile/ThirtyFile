//! Requests to the server's API, and the errors they fail with

import { t, tServer } from "@/lib/i18n";
import { isNetworkError, unreachable } from "@/lib/utils";

export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
    public code?: string,
    /** The server's id of the failed request (X-Request-Id), which ties an error report to the server's record of it */
    public requestId?: string,
    /** The server's own message, before it was translated for the page: error reports send this one (the server
     * knows how to leave the names out of it) */
    public serverMessage?: string,
  ) {
    super(message);
  }
}

/**
 * fetch(), except that a server that can't be reached fails with an ApiError (status 0) saying so in the interface's
 * language, rather than the browser's "Failed to fetch". A cancelled request still fails with its AbortError.
 */
export async function send(url: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(url, init);
  } catch (e) {
    throw isNetworkError(e) ? new ApiError(unreachable(), 0, "unreachable") : e;
  }
}

export async function request<T>(method: string, path: string, body?: unknown, raw?: BodyInit, extraHeaders?: Record<string, string>, signal?: AbortSignal): Promise<T> {
  const res = await send(`/api${path}`, {
    method,
    signal,
    credentials: "same-origin",
    headers: {
      ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      ...extraHeaders,
    },
    body: body !== undefined ? JSON.stringify(body) : raw,
  });
  if (!res.ok) throw await responseError(res, `/api${path}`, t("Request failed ({status})", { status: res.status }));
  return res.json() as Promise<T>;
}

/**
 * The error for a failed response: the server's message translated to the UI language, or `fallback`.
 * A 401 without a code means the session expired: the user is sent to sign in (except for sign-in attempts and public share links).
 */
export function errorFromBody(status: number, body: string, url: string, fallback: string, requestId?: string): ApiError {
  let message = fallback;
  let code: string | undefined;
  let serverMessage: string | undefined;
  try {
    const data = JSON.parse(body);
    // Server messages are English: translate them to the UI language
    if (typeof data.error === "string" && data.error) {
      serverMessage = data.error;
      message = tServer(data.error);
    }
    code = data.code;
  } catch {
    // Non-JSON error
  }
  if (status === 401 && code === undefined && !url.startsWith("/api/auth/login") && !url.startsWith("/api/public/")) {
    window.dispatchEvent(new Event("tf:unauthorized"));
  }
  return new ApiError(message, status, code, requestId, serverMessage);
}

export async function responseError(res: Response, url: string, fallback: string): Promise<ApiError> {
  return errorFromBody(res.status, await res.text().catch(() => ""), url, fallback, res.headers.get("x-request-id") ?? undefined);
}

/** `fetch` for file content and other requests made outside `api`, with the same error handling: throws an ApiError when the response isn't OK */
export async function fetchOk(url: string, init?: RequestInit): Promise<Response> {
  const res = await send(url, { credentials: "same-origin", ...init });
  if (!res.ok) throw await responseError(res, url, t("Couldn't read the file ({status})", { status: res.status }));
  return res;
}

/** An Office file's content (.docx / .xlsx / .pptx), checked to be an Office Open XML package */
export async function fetchOffice(url: string, signal?: AbortSignal): Promise<ArrayBuffer> {
  return checkOoxml(await (await fetchOk(url, { signal })).arrayBuffer());
}

/** .docx / .xlsx / .pptx are really ZIP archives; check the header first so the preview and editor don't throw a cryptic error or show a blank page */
function checkOoxml(buf: ArrayBuffer) {
  const b = new Uint8Array(buf.slice(0, 4));
  if (b[0] === 0x50 && b[1] === 0x4b && b[2] === 0x03 && b[3] === 0x04) return buf;
  if (b[0] === 0xd0 && b[1] === 0xcf && b[2] === 0x11 && b[3] === 0xe0)
    throw new ApiError(t("This is a legacy Office file (.doc / .xls / .ppt) with a newer file extension, so it can't be opened online. Download it and open it in Office."), 0);
  if (buf.byteLength === 0) throw new ApiError(t("This file is empty."), 0);
  throw new ApiError(t("This file isn't a valid Office document (it may be damaged, or wasn't created by Office), so it can't be opened online. Download it to check."), 0);
}

/** `signal` stops the request when its answer isn't wanted any more (React Query passes one to each query) */
export const get = <T>(p: string, signal?: AbortSignal) => request<T>("GET", p, undefined, undefined, undefined, signal);
/** Convert filters to query parameters (skipping empty values) */
export const toParams = (o: object) =>
  Object.fromEntries(Object.entries(o).flatMap(([k, v]) => (v === undefined || v === null || v === "" ? [] : [[k, String(v)]]))) as Record<string, string>;
export const post = <T>(p: string, body?: unknown) => request<T>("POST", p, body ?? {});
/**
 * API path with every interpolated part encoded as one path segment (or query value): ids and share tokens can come from
 * the address bar, and `/share/abc%3Fx=1` must not turn into `/public/shares/abc?x=1/unlock`. Query strings built with qs()
 * are added after it, unencoded.
 */
export const enc = (strings: TemplateStringsArray, ...parts: (string | number)[]) =>
  strings.reduce((out, s, i) => out + s + (i < parts.length ? encodeURIComponent(String(parts[i])) : ""), "");
export const qs = (params: Record<string, string | undefined>) => {
  const s = new URLSearchParams(Object.entries(params).filter((e): e is [string, string] => e[1] !== undefined));
  const str = s.toString();
  return str ? `?${str}` : "";
};
