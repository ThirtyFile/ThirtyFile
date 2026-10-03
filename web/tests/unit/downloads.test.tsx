import { act, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { DownloadPanel } from "@/components/DownloadPanel";
import { cancelDownload, clearDownloads, download, retryDownload, triggerDownload, useDownloads, type DownloadTask } from "@/downloads";

const notice = vi.hoisted(() => vi.fn<(message: string) => void>());
vi.mock("sonner", () => ({ toast: { info: notice } }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let shown: DownloadTask[];
let root: Root;
let el: HTMLDivElement;
const fetcher = vi.fn<typeof fetch>();
function Probe() {
  const tasks = useDownloads();
  useLayoutEffect(() => {
    shown = tasks;
  }, [tasks]);
  return <DownloadPanel />;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
beforeEach(async () => {
  el = document.createElement("div");
  document.body.append(el);
  root = createRoot(el);
  fetcher.mockReset();
  notice.mockClear();
  vi.stubGlobal("fetch", fetcher);
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:download-test");
  await act(async () => root.render(<Probe />));
});
afterEach(async () => {
  await act(async () => {
    clearDownloads();
    root.unmount();
  });
  el.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

test("a pending link immediately shows cancellable preparation without an invented percentage", async () => {
  const link = deferred<string>();
  let sending!: Promise<void>;
  await act(async () => {
    sending = download(() => link.promise, { name: "archive.zip", zip: true });
  });
  expect(shown[0].status).toBe("preparing");
  expect(el.textContent).toContain("Preparing download…");
  expect(el.textContent).not.toContain("0%");
  expect(el.querySelector('[aria-label="Download progress"]')?.hasAttribute("aria-valuenow")).toBe(false);
  expect(el.querySelector('[aria-label="Cancel download"]')).not.toBeNull();
  await act(async () => cancelDownload(shown[0].id));
  expect(shown[0].status).toBe("canceled");
  await act(async () => {
    link.resolve("/late-link");
    await sending;
  });
  expect(fetcher).not.toHaveBeenCalled();
  expect(HTMLAnchorElement.prototype.click).not.toHaveBeenCalled();
});

test("canceling after the link resolves still aborts the request waiting for headers", async () => {
  const headers = deferred<Response>();
  fetcher.mockReturnValue(headers.promise);
  let sending!: Promise<void>;
  await act(async () => {
    sending = download("/slow");
  });
  const task = shown[0];
  expect(task.status).toBe("preparing");
  await act(async () => cancelDownload(task.id));
  expect(task.controller.signal.aborted).toBe(true);
  await act(async () => {
    headers.resolve(new Response("data"));
    await sending;
  });
  expect(shown[0].status).toBe("canceled");
  expect(HTMLAnchorElement.prototype.click).not.toHaveBeenCalled();
});

test("failed link preparation remains retryable and retries obtain a fresh link", async () => {
  const failed = deferred<string>();
  const retried = deferred<string>();
  const source = vi.fn<(signal?: AbortSignal) => Promise<string>>().mockReturnValueOnce(failed.promise).mockReturnValueOnce(retried.promise);
  let sending!: Promise<void>;
  await act(async () => {
    sending = download(source, { name: "bundle" });
  });
  await act(async () => {
    failed.reject(new Error("Cannot prepare archive"));
    await sending;
  });
  expect(shown[0]).toMatchObject({ status: "error", error: "Cannot prepare archive" });
  await act(async () => retryDownload(shown[0].id));
  expect(source).toHaveBeenCalledTimes(2);
  expect(shown[0].status).toBe("preparing");
  await act(async () => clearDownloads());
  await act(async () => retried.resolve("/unused"));
  expect(fetcher).not.toHaveBeenCalled();
  expect(shown).toHaveLength(0);
});

test("an unknown-size stream keeps aggregate progress indeterminate until completion", async () => {
  let stream!: ReadableStreamDefaultController<Uint8Array>;
  fetcher.mockResolvedValue(
    new Response(
      new ReadableStream<Uint8Array>({
        start(controller) {
          stream = controller;
        },
      }),
    ),
  );
  let sending!: Promise<void>;
  await act(async () => {
    sending = download("/stream", { name: "stream.bin" });
  });
  await act(async () => stream.enqueue(new Uint8Array([1, 2, 3])));
  expect(shown[0].status).toBe("downloading");
  expect(el.querySelector('[aria-label="Download progress"]')?.hasAttribute("aria-valuenow")).toBe(false);
  expect(el.textContent).not.toContain("100%");
  await act(async () => {
    stream.close();
    await sending;
  });
  expect(shown[0]).toMatchObject({ status: "done", received: 3 });
  expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledTimes(1);
});

test("large files are handed to the browser once with one notice", async () => {
  fetcher.mockResolvedValue(new Response("unused", { headers: { "content-length": String(300 * 1024 ** 2) } }));
  await act(async () => {
    await download("/large");
  });
  expect(shown).toHaveLength(0);
  expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledTimes(1);
  expect(notice).toHaveBeenCalledTimes(1);
  expect(notice.mock.calls[0][0]).toContain("Large file");
});

test("public downloads stay one native request and are never fetched twice", async () => {
  await act(async () => {
    await triggerDownload("/api/public/token/download");
  });
  expect(fetcher).not.toHaveBeenCalled();
  expect(shown).toHaveLength(0);
  expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledTimes(1);
  expect(notice).toHaveBeenCalledTimes(1);
});

test("public archive preparation can be canceled and a completed preparation stays native", async () => {
  const link = deferred<string>();
  let sending!: Promise<void>;
  await act(async () => {
    sending = download(() => link.promise, { native: true });
  });
  expect(shown[0].status).toBe("preparing");
  await act(async () => cancelDownload(shown[0].id));
  await act(async () => {
    link.resolve("/api/public/late");
    await sending;
  });
  expect(HTMLAnchorElement.prototype.click).not.toHaveBeenCalled();
  await act(async () => {
    await download(async () => "/api/public/archive", { native: true });
  });
  expect(fetcher).not.toHaveBeenCalled();
  expect(HTMLAnchorElement.prototype.click).toHaveBeenCalledTimes(1);
});
