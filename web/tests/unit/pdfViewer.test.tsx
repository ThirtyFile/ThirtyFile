// The PDF viewer (components/pdf/PdfViewer.tsx), with pdf.js stood in for: what it shows while a file opens, asks for a
// password, can't be read, and what its toolbar does once pages are shown
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import type { FileSource, Node } from "@/api";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** A document being opened, settled by the test */
interface FakeTask {
  promise: Promise<unknown>;
  resolve(doc: unknown): void;
  reject(e: unknown): void;
  onPassword: ((update: (p: string) => void, reason: number) => void) | null;
  destroy: ReturnType<typeof vi.fn>;
}
const opened = vi.hoisted(() => [] as FakeTask[]);
/** pdf.js's viewer as made by the component, and the events it was sent */
const made = vi.hoisted(() => ({
  viewer: null as null | Record<string, unknown>,
  sent: [] as { name: string; data: Record<string, unknown> }[],
  listeners: new Map<string, (e: unknown) => void>(),
}));

vi.mock("@/lib/pdfjs", () => {
  class EventBus {
    on(name: string, fn: (e: unknown) => void) {
      made.listeners.set(name, fn);
    }
    dispatch(name: string, data: Record<string, unknown>) {
      made.sent.push({ name, data });
    }
  }
  class PDFViewer {
    currentPageNumber = 1;
    currentScaleValue = "";
    setDocument = vi.fn<(doc: unknown) => void>();
    cleanup = vi.fn<() => void>();
    updateScale = vi.fn<(o: unknown) => void>();
    constructor() {
      made.viewer = this as unknown as Record<string, unknown>;
    }
  }
  class PDFLinkService {
    setViewer() {}
    setDocument() {}
  }
  class PDFFindController {}
  return {
    pdfjs: {
      getDocument: () => {
        let resolve!: (d: unknown) => void;
        let reject!: (e: unknown) => void;
        const promise = new Promise((res, rej) => {
          resolve = res;
          reject = rej;
        });
        // A rejection the component hasn't subscribed to yet isn't an error of the test
        promise.catch(() => undefined);
        const task: FakeTask = { promise, resolve, reject, onPassword: null, destroy: vi.fn<() => Promise<void>>(async () => undefined) };
        opened.push(task);
        return task;
      },
      PasswordResponses: { NEED_PASSWORD: 1, INCORRECT_PASSWORD: 2 },
      AnnotationMode: { ENABLE: 1 },
      TouchManager: class {},
    },
    documentOptions: () => ({}),
    UNREADABLE: ["InvalidPDFException", "PasswordException", "FormatError"],
    loadViewerComponents: async () => ({ EventBus, PDFViewer, PDFLinkService, PDFFindController, LinkTarget: { BLANK: 2 } }),
  };
});
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));
const triggerDownload = vi.hoisted(() => vi.fn<(url: string) => void>());
vi.mock("@/downloads", () => ({ triggerDownload }));

const { default: PdfViewer } = await import("@/components/pdf/PdfViewer");

const node: Node = {
  id: "pdf-1",
  parent_id: "folder",
  kind: "file",
  name: "minutes.pdf",
  size: 1000,
  mime: "application/pdf",
  created_at: 0,
  updated_at: 0,
} as Node;
const source: FileSource = { contentUrl: (n, download) => `/api/files/${n.id}/content${download ? "?download=1" : ""}`, thumbUrl: () => "", downloadLink: () => Promise.resolve("") };

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  opened.length = 0;
  made.viewer = null;
  made.sent.length = 0;
  made.listeners.clear();
  triggerDownload.mockClear();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const show = (props: Partial<Parameters<typeof PdfViewer>[0]> = {}) => act(async () => root.render(<PdfViewer node={node} source={source} embedded {...props} />));
const button = (name: string) => host.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`);
function typeInto(el: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}
const flush = () => act(async () => await new Promise((r) => setTimeout(r, 0)));

describe("opening a PDF", () => {
  test("reads the file's content without asking for a download", async () => {
    await show();
    expect(opened).toHaveLength(1);
    expect(host.querySelector('[aria-label="Loading…"]')).not.toBeNull();
  });

  test("asks for the password of a protected file, and again when it was wrong", async () => {
    await show();
    const update = vi.fn<(p: string) => void>();
    await act(async () => opened[0].onPassword?.(update, 1));
    const field = host.querySelector<HTMLInputElement>('input[type="password"]');
    expect(field).not.toBeNull();
    expect(host.querySelector('[role="alert"]')).toBeNull();
    await act(async () => typeInto(field!, "secret"));
    await act(async () => host.querySelector("form")?.requestSubmit());
    expect(update).toHaveBeenCalledWith("secret");

    await act(async () => opened[0].onPassword?.(update, 2));
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("That password isn't right");
    // The form can be sent again
    await act(async () => typeInto(host.querySelector<HTMLInputElement>('input[type="password"]')!, "secret2"));
    expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(false);
  });

  test("says a damaged file can't be shown, and offers it for download", async () => {
    await show();
    await act(async () => {
      const e = new Error("Invalid PDF structure.");
      e.name = "InvalidPDFException";
      opened[0].reject(e);
      await opened[0].promise.catch(() => undefined);
    });
    expect(host.textContent).toContain("This file is damaged or isn't a PDF");
    host.querySelector<HTMLButtonElement>('[role="alert"] button')?.click();
    expect(triggerDownload).toHaveBeenCalledWith("/api/files/pdf-1/content?download=1");
  });

  test("offers no download where the link doesn't allow one", async () => {
    await show({ allowDownload: false });
    await act(async () => {
      opened[0].reject(new Error("Network error"));
      await opened[0].promise.catch(() => undefined);
    });
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("Network error");
    expect(host.querySelector('[role="alert"] button')).toBeNull();
  });

  test("stops reading the file when it is closed", async () => {
    await show();
    act(() => root.unmount());
    expect(opened[0].destroy).toHaveBeenCalled();
    root = createRoot(host);
  });
});

describe("pages shown", () => {
  async function ready(props: Partial<Parameters<typeof PdfViewer>[0]> = {}, numPages = 1) {
    await show(props);
    await act(async () => {
      opened[0].resolve({ numPages, getPage: () => new Promise(() => undefined) });
      await opened[0].promise;
    });
    await flush();
    expect(made.viewer).not.toBeNull();
    await act(async () => made.listeners.get("pagesinit")?.({}));
  }

  test("start at automatic zoom and go to a page typed in", async () => {
    await ready({}, 5);
    expect(made.viewer?.currentScaleValue).toBe("auto");
    const page = host.querySelector<HTMLInputElement>('input[aria-label="Page number"]')!;
    await act(async () => typeInto(page, "9"));
    await act(async () => page.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(made.viewer?.currentPageNumber).toBe(5);
    expect(host.textContent).toContain("of 5");
  });

  test("opened from a search inside files, go to the words searched for and count them", async () => {
    await ready({ find: '"budget" tender' });
    const find = made.sent.find((s) => s.name === "find");
    expect(find?.data).toMatchObject({ query: ["budget", "tender"], highlightAll: true, findPrevious: false });
    expect(host.querySelector<HTMLInputElement>('input[type="search"]')?.value).toBe("budget tender");
    await act(async () => made.listeners.get("updatefindcontrolstate")?.({ state: 0, matchesCount: { current: 1, total: 4 } }));
    expect(host.querySelector('[role="status"]')?.textContent).toContain("Match 1 of 4");
    // Enter goes on to the next match, Shift+Enter back
    const box = host.querySelector<HTMLInputElement>('input[type="search"]')!;
    await act(async () => box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", shiftKey: true, bubbles: true })));
    expect(made.sent.at(-1)?.data).toMatchObject({ type: "again", findPrevious: true, query: ["budget", "tender"] });
  });

  test("say when what is typed in the find box isn't there", async () => {
    await ready();
    await act(async () => typeInto(host.querySelector<HTMLInputElement>('input[type="search"]')!, "nowhere"));
    expect(made.sent.at(-1)?.data).toMatchObject({ type: "", query: "nowhere" });
    await act(async () => made.listeners.get("updatefindcontrolstate")?.({ state: 1, matchesCount: { current: 0, total: 0 } }));
    expect(host.querySelector('[role="status"]')?.textContent).toContain("Not found");
  });

  test("offer Print and Download only where downloading is allowed", async () => {
    await ready();
    expect(button("Print")).not.toBeNull();
    button("Download")?.click();
    expect(triggerDownload).toHaveBeenCalledWith("/api/files/pdf-1/content?download=1");

    act(() => root.unmount());
    root = createRoot(host);
    opened.length = 0;
    await ready({ allowDownload: false });
    expect(button("Print")).toBeNull();
    expect(button("Download")).toBeNull();
  });

  test("zoom in and out a step at a time", async () => {
    await ready();
    await act(async () => made.listeners.get("scalechanging")?.({ scale: 1, presetValue: "auto" }));
    button("Zoom in")?.click();
    expect(made.viewer?.currentScaleValue).toBe("1.25");
    button("Zoom out")?.click();
    expect(made.viewer?.currentScaleValue).toBe("0.75");
  });
});
