// Requests that can't reach the server (api/client.ts, lib/utils.ts): they say so in the interface's language rather
// than with the browser's own "Failed to fetch"
import { afterEach, describe, expect, test, vi } from "vitest";
import { ApiError } from "@/api";
import { fetchOk, request } from "@/api/client";
import { errorMessage, isNetworkError, unreachable } from "@/lib/utils";

afterEach(() => vi.unstubAllGlobals());

const failWith = (e: unknown) =>
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(() => Promise.reject(e)),
  );

describe("a server that can't be reached", () => {
  // Chrome, Firefox and Safari
  for (const message of ["Failed to fetch", "NetworkError when attempting to fetch resource.", "Load failed"]) {
    test(`"${message}" becomes a translated message`, async () => {
      failWith(new TypeError(message));
      const e = await request("GET", "/drives").catch((e: unknown) => e);
      expect(e).toBeInstanceOf(ApiError);
      expect((e as ApiError).message).toBe(unreachable());
      expect((e as ApiError).message).not.toContain(message);
      expect((e as ApiError).status).toBe(0);
      await expect(fetchOk("/api/files/x/content")).rejects.toThrow(unreachable());
      expect(errorMessage(new TypeError(message), "fallback")).toBe(unreachable());
    });
  }

  test("a cancelled request still fails as cancelled, and other errors keep their message", async () => {
    const abort = new DOMException("The operation was aborted.", "AbortError");
    failWith(abort);
    await expect(request("GET", "/drives")).rejects.toBe(abort);
    expect(isNetworkError(abort)).toBe(false);
    expect(errorMessage(new TypeError("x is not a function"), "fallback")).toBe("x is not a function");
    expect(errorMessage("thrown text", "fallback")).toBe("fallback");
  });
});
