// lib/cancellable.ts: loading an effect stops when it cleans up
import { describe, expect, test, vi } from "vitest";
import { cancellable } from "@/lib/cancellable";

describe("cancellable", () => {
  test("gives the result, or the error, while it is still wanted", async () => {
    const done = vi.fn<(v: string) => void>();
    const failed = vi.fn<(e: Error) => void>();
    cancellable(() => Promise.resolve("content"), done, failed);
    cancellable(() => Promise.reject(new Error("Couldn't read the file (500)")), done, failed);
    await new Promise((r) => setTimeout(r));
    expect(done).toHaveBeenCalledWith("content");
    expect(failed).toHaveBeenCalledWith(new Error("Couldn't read the file (500)"));
  });

  test("stopping aborts the request, and neither handler is called after", async () => {
    let signal: AbortSignal | undefined;
    const done = vi.fn<(v: string) => void>();
    const failed = vi.fn<(e: Error) => void>();
    const stop = cancellable(
      (s) => {
        signal = s;
        return new Promise<string>((resolve, reject) => {
          s.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
          setTimeout(() => resolve("late"), 10);
        });
      },
      done,
      failed,
    );
    stop();
    expect(signal?.aborted).toBe(true);
    await new Promise((r) => setTimeout(r, 20));
    expect(done).not.toHaveBeenCalled();
    expect(failed).not.toHaveBeenCalled();
  });
});
