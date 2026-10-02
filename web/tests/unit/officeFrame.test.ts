// The sandboxed preview frame (office-frame.ts): it takes work only from the page that embeds it, checks the shape of
// every message, and hands links back to that page instead of following them
import { afterAll, beforeEach, describe, expect, test, vi } from "vitest";
import { buildWorkbook } from "../fixtures";

const renderDocx = vi.hoisted(() => vi.fn<(buffer: ArrayBuffer, root: HTMLElement) => Promise<{ dispose(): void }>>());
const renderPptx = vi.hoisted(() => vi.fn<(buffer: ArrayBuffer, root: HTMLElement) => Promise<{ dispose(): void }>>());
vi.mock("@/ooxml/docx", () => ({ renderDocx }));
vi.mock("@/ooxml/pptx", () => ({ renderPptx }));

// In the tests the frame is the top window, so its parent is itself: what it says to the parent is caught here
const replies: unknown[] = [];
const toParent = vi.spyOn(window.parent, "postMessage").mockImplementation((msg: unknown) => void replies.push(msg));
document.body.innerHTML = '<div id="root"></div>';
await import("@/office-frame");
const root = document.getElementById("root")!;

afterAll(() => toParent.mockRestore());

/** A message arriving from `source` (the embedding page unless said otherwise) */
function send(data: unknown, source: MessageEventSource | null = window.parent) {
  window.dispatchEvent(new MessageEvent("message", { data, source }));
}

/** Waits for the queue of work to answer, and returns what it said */
async function answer() {
  await vi.waitFor(() => expect(replies.length).toBeGreaterThan(0));
  return replies.splice(0);
}

/** Lets anything queued run, for messages that must not start anything */
async function settle() {
  for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
}

beforeEach(() => {
  renderDocx.mockReset();
  renderPptx.mockReset();
  renderDocx.mockImplementation(async (_, el) => {
    el.textContent = "document";
    return { dispose: vi.fn<() => void>() };
  });
  renderPptx.mockImplementation(async (_, el) => {
    el.textContent = "slides";
    return { dispose: vi.fn<() => void>() };
  });
});

describe("the preview frame", () => {
  test("says it is ready once its script runs", () => {
    expect(replies.splice(0)).toEqual([{ type: "ready" }]);
  });

  test("renders a document sent by the page that embeds it, and says when it's done", async () => {
    send({ type: "render", kind: "docx", buffer: new ArrayBuffer(4) });
    expect(await answer()).toEqual([{ type: "done" }]);
    expect(renderDocx).toHaveBeenCalledTimes(1);
    expect(root.textContent).toBe("document");

    // The next document replaces it, and the previous one is let go
    const first = await renderDocx.mock.results[0].value;
    send({ type: "render", kind: "pptx", buffer: new ArrayBuffer(4) });
    expect(await answer()).toEqual([{ type: "done" }]);
    expect(first.dispose).toHaveBeenCalled();
    expect(root.textContent).toBe("slides");
  });

  test("ignores messages from anywhere else: another frame, a window it opened, or none", async () => {
    const other = { postMessage: vi.fn<() => void>() } as unknown as MessageEventSource;
    send({ type: "render", kind: "docx", buffer: new ArrayBuffer(4) }, other);
    send({ type: "render", kind: "docx", buffer: new ArrayBuffer(4) }, null);
    await settle();
    expect(renderDocx).not.toHaveBeenCalled();
    expect(replies).toEqual([]);
  });

  test("ignores messages of the wrong shape", async () => {
    for (const data of [
      null,
      "render",
      { type: "render", kind: "docx", buffer: "not a buffer" },
      { type: "render", kind: "exe", buffer: new ArrayBuffer(4) },
      { type: "load", kind: "docx", buffer: new ArrayBuffer(4) },
      { type: "render", kind: "xlsx-drawings", sheet: 1, cols: new Float64Array(0), rows: new Float64Array(0) },
      { type: "render", kind: "xlsx-drawings", sheet: "xl/worksheets/sheet1.xml", cols: [0], rows: [0] },
      { type: "scroll", x: "0", y: 0 },
    ])
      send(data);
    await settle();
    expect(renderDocx).not.toHaveBeenCalled();
    expect(replies).toEqual([]);
  });

  test("reports a document it can't render", async () => {
    renderDocx.mockRejectedValueOnce(new Error("bad document"));
    send({ type: "render", kind: "docx", buffer: new ArrayBuffer(4) });
    expect(await answer()).toEqual([{ type: "error", message: "bad document" }]);
  });

  test("draws a workbook's sheets only once the workbook is loaded, and says when a drawing came without one", async () => {
    const drawings = { type: "render", kind: "xlsx-drawings", sheet: "xl/worksheets/sheet1.xml", cols: new Float64Array([0, 64]), rows: new Float64Array([0, 20]) };
    // Loading starts again from nothing (the document before is let go)
    const buffer = await buildWorkbook([{ name: "Data", rows: '<row r="1"><c r="A1"><v>1</v></c></row>' }]);
    send({ type: "load", kind: "xlsx", buffer });
    send(drawings);
    send({ type: "scroll", x: 10, y: 20 });
    await vi.waitFor(() => expect(replies).toHaveLength(2));
    expect(replies.splice(0)).toEqual([{ type: "done" }, { type: "done" }]);
    // The panes of the drawing layer follow the scroll
    const panes = root.querySelectorAll<HTMLElement>("div > div > div");
    expect([...panes].some((p) => p.style.transform === "translate(-10px,-20px)")).toBe(true);

    // A document replaces the workbook: drawings asked for after that have nothing to draw
    send({ type: "render", kind: "docx", buffer: new ArrayBuffer(4) });
    send(drawings);
    await vi.waitFor(() => expect(replies).toHaveLength(2));
    expect(replies.splice(0)).toEqual([{ type: "done" }, { type: "error", message: "workbook not loaded" }]);
  });

  test("hands web and email links to the page, follows bookmarks itself, and drops anything else", async () => {
    root.innerHTML =
      '<a href="https://example.com/a">web</a><a href="mailto:amy@example.com">mail</a><a href="javascript:alert(1)">script</a><a href="#part">bookmark</a><p id="part">Part</p>';
    const target = root.querySelector<HTMLElement>("#part")!;
    target.scrollIntoView = vi.fn<() => void>();
    const click = (i: number) => {
      const event = new MouseEvent("click", { bubbles: true, cancelable: true });
      root.querySelectorAll("a")[i].dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect([click(0), click(1), click(2), click(3)]).toEqual([true, true, true, true]);
    expect(replies.splice(0)).toEqual([
      { type: "link", href: "https://example.com/a" },
      { type: "link", href: "mailto:amy@example.com" },
    ]);
    expect(target.scrollIntoView).toHaveBeenCalled();
  });
});
