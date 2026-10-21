// The page's side of the sandboxed preview frames: Word / PowerPoint previews (OfficeViewer.tsx) and the drawing layer of
// Excel previews (sheet/SheetPreview.tsx) only listen to their own frame, and a link from a document opens only right
// after a click. The frame is stood in for: its window is a fake that records what the page sends it.
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import JSZip from "jszip";
import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from "vitest";
import type { FileSource, Node } from "@/api";
import { buildWorkbook } from "../fixtures";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const fetchOffice = vi.hoisted(() => vi.fn<(url: string, signal?: AbortSignal) => Promise<ArrayBuffer>>());
vi.mock("@/api", async (importOriginal) => ({ ...(await importOriginal<typeof import("@/api")>()), fetchOffice }));
vi.mock("@/components/officeFrame", () => ({
  loadFrameScript: () => Promise.resolve("/* the renderer */"),
  frameDocument: () => "<!doctype html><html><body></body></html>",
}));
vi.mock("@/lib/errorReport", () => ({ reportShown: vi.fn<() => void>() }));
vi.mock("@/components/office/OfficeWorkspace", () => ({
  default: (props: { node: Node; editable?: boolean; allowDownload?: boolean }) => (
    <div data-office-stub={props.node.name} data-editable={!!props.editable} data-export={props.allowDownload !== false} />
  ),
}));

const { default: OfficeViewer } = await import("@/components/OfficeViewer");
const { default: SheetPreview } = await import("@/components/sheet/SheetPreview");

/** Each frame's window: records what the page posts to it */
type FakeWindow = { postMessage: ReturnType<typeof vi.fn<(msg: unknown, origin: string, transfer?: Transferable[]) => void>> };
const windows = new WeakMap<HTMLIFrameElement, FakeWindow>();
const contentWindow = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, "contentWindow");
beforeAll(() => {
  Object.defineProperty(HTMLIFrameElement.prototype, "contentWindow", {
    configurable: true,
    get(this: HTMLIFrameElement) {
      if (!windows.has(this)) windows.set(this, { postMessage: vi.fn<(msg: unknown, origin: string, transfer?: Transferable[]) => void>() });
      return windows.get(this);
    },
  });
  return () => {
    if (contentWindow) Object.defineProperty(HTMLIFrameElement.prototype, "contentWindow", contentWindow);
  };
});

const source: FileSource = { contentUrl: (n) => `/api/files/${n.id}/content`, viewUrl: () => "/view", thumbUrl: () => "", downloadLink: () => Promise.resolve("") };
const node = (name: string): Node => ({
  id: name,
  parent_id: "folder",
  kind: "file",
  name,
  size: 1024,
  mime: "application/octet-stream",
  created_at: 1_700_000_000,
  updated_at: 1_700_000_000,
  trashed_at: null,
  drive_id: "drive",
  owner_name: "amy",
  is_favorite: false,
});

let root: Root | null = null;
let container: HTMLElement;
async function mount(el: React.ReactElement) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root!.render(el));
}

/** The frame, once the page has put it there */
async function frame() {
  let el: HTMLIFrameElement | null = null;
  await vi.waitFor(() => {
    el = container.querySelector("iframe");
    expect(el).not.toBeNull();
  });
  return { el: el!, win: windows.get(el!) ?? (el!.contentWindow as unknown as FakeWindow) };
}

/** A message to the page from `source` */
async function message(data: unknown, from: unknown) {
  await act(async () => {
    window.dispatchEvent(new MessageEvent("message", { data, source: from as MessageEventSource }));
  });
}

const activation = (isActive: boolean | undefined) => Object.defineProperty(navigator, "userActivation", { configurable: true, value: isActive === undefined ? undefined : { isActive } });

beforeEach(() => {
  fetchOffice.mockReset();
  fetchOffice.mockResolvedValue(new ArrayBuffer(8));
});

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container.remove();
  activation(undefined);
  vi.restoreAllMocks();
});

describe("packaged Office previews", () => {
  test("uses the same readonly SDK surface for Word, Excel and PPT without an iframe", async () => {
    for (const name of ["Plans.docx", "Budget.xlsx", "Deck.pptx"]) {
      await mount(<OfficeViewer node={node(name)} source={source} />);
      expect(container.querySelector("[data-office-stub]")?.getAttribute("data-office-stub")).toBe(name);
      expect(container.querySelector("[data-office-stub]")?.getAttribute("data-editable")).toBe("false");
      expect(container.querySelector("iframe")).toBeNull();
    }
  });
  test("preserves a share's no-download policy in the SDK", async () => {
    await mount(<OfficeViewer node={node("Deck.pptx")} source={source} allowDownload={false} />);
    expect(container.querySelector("[data-office-stub]")?.getAttribute("data-export")).toBe("false");
  });
  test("does not initialize a Worker for a file over the preview limit", async () => {
    await mount(<OfficeViewer node={{ ...node("Large.docx"), size: 51 * 1024 * 1024 }} source={source} />);
    expect(container.textContent).toContain("too large");
    expect(container.querySelector("[data-office-stub]")).toBeNull();
  });
});
describe("the drawing layer of a spreadsheet preview", () => {
  /** A workbook with a drawing part, so the preview loads its drawing frame */
  async function workbookWithDrawings() {
    const zip = await JSZip.loadAsync(await buildWorkbook([{ name: "Data", rows: '<row r="1"><c r="A1"><v>1</v></c></row>' }]));
    zip.file("xl/drawings/drawing1.xml", '<?xml version="1.0"?><xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"/>');
    return zip.generateAsync({ type: "arraybuffer" });
  }

  test("sends the drawing parts and the sheet's positions only to its own frame, in order", async () => {
    const onError = vi.fn<(m: string) => void>();
    await mount(<SheetPreview buffer={await workbookWithDrawings()} onError={onError} />);
    const { win } = await frame();

    await message({ type: "ready" }, window);
    expect(win.postMessage).not.toHaveBeenCalled();
    await message({ type: "ready" }, win);
    expect(win.postMessage).toHaveBeenCalledTimes(1);
    const [load, , transfer] = win.postMessage.mock.calls[0];
    expect(load).toMatchObject({ type: "load", kind: "xlsx" });
    const parts = (load as { buffer: ArrayBuffer }).buffer;
    expect(transfer).toEqual([parts]);
    // Only what drawing needs: no cell data
    const files = Object.keys((await JSZip.loadAsync(parts)).files);
    expect(files).toContain("xl/drawings/drawing1.xml");
    expect(files.some((f) => f.startsWith("xl/worksheets/") || f === "xl/sharedStrings.xml")).toBe(false);

    // Once the frame has the workbook, the sheet shown is drawn and scrolled to where the grid is
    await message({ type: "done" }, window);
    expect(win.postMessage).toHaveBeenCalledTimes(1);
    await message({ type: "done" }, win);
    expect(win.postMessage.mock.calls.slice(1).map(([m]) => m)).toEqual([
      { type: "render", kind: "xlsx-drawings", sheet: "xl/worksheets/sheet1.xml", cols: expect.any(Float64Array), rows: expect.any(Float64Array), frozen: { rows: 0, cols: 0 } },
      { type: "scroll", x: 0, y: 0 },
    ]);
    // The next "done" answers that drawing: it isn't sent again
    await message({ type: "done" }, win);
    expect(win.postMessage).toHaveBeenCalledTimes(3);
    expect(onError).not.toHaveBeenCalled();
  });

  test("isn't loaded at all for a workbook with nothing to draw", async () => {
    await mount(<SheetPreview buffer={await buildWorkbook([{ name: "Data", rows: "" }])} onError={() => {}} />);
    await vi.waitFor(() => expect(container.querySelector("canvas")).not.toBeNull());
    expect(container.querySelector("iframe")).toBeNull();
  });
});
