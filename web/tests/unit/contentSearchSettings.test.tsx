// Control panel › General › Search: turning searching inside files on, how far the reading has got, the limits and indexing again
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ContentSearchInfo } from "@/api";
import { ContentSearchSettings } from "@/admin/system/ContentSearchSettings";
import { Snippet } from "@/components/Snippet";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const requests = vi.hoisted(() => ({
  contentSearch: vi.fn<typeof import("@/api").api.contentSearch>(),
  saveContentSearch: vi.fn<typeof import("@/api").api.saveContentSearch>(),
  rebuildContentSearch: vi.fn<typeof import("@/api").api.rebuildContentSearch>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

const off: ContentSearchInfo = { enabled: false, paused: false, max_file_mb: 25, max_total_mb: 500, queued: 0, indexed: 0, skipped: 0, failed: 0, bytes: 0, full: false };
const reading: ContentSearchInfo = { ...off, enabled: true, queued: 30, indexed: 60, skipped: 8, failed: 2, bytes: 5 * 1024 * 1024 };

let root: Root;
let host: HTMLElement;
function show(info: ContentSearchInfo) {
  requests.contentSearch.mockResolvedValue(info);
  // A new page each time: one query client per page
  act(() => root.unmount());
  root = createRoot(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  act(() =>
    root.render(
      <QueryClientProvider client={qc}>
        <ContentSearchSettings />
      </QueryClientProvider>,
    ),
  );
}
beforeEach(() => {
  Object.values(requests).forEach((r) => r.mockReset());
  requests.saveContentSearch.mockImplementation(async (s) => ({ ...reading, ...s }));
  requests.rebuildContentSearch.mockResolvedValue({ ...reading, indexed: 0, queued: 100 });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});
const settle = (check: () => void) => act(async () => vi.waitFor(check));
const text = () => document.body.textContent ?? "";
const button = (name: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === name)!;
const toggle = (label: string) => document.querySelector<HTMLElement>(`[role="switch"][aria-label="${label}"]`)!;
async function type(selector: string, value: string) {
  const el = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

test("it is off until an administrator turns it on, and turning it on sends the limits as they are", async () => {
  show(off);
  await settle(() => expect(toggle("Search inside files")).not.toBeNull());
  expect(toggle("Search inside files").getAttribute("aria-checked")).toBe("false");
  expect(text()).not.toContain("Index again");
  await act(async () => toggle("Search inside files").click());
  await settle(() => expect(requests.saveContentSearch).toHaveBeenCalledWith({ enabled: true, paused: false, max_file_mb: 25, max_total_mb: 500 }));
});

test("the reading is shown with its progress, what holds nothing and what failed", async () => {
  show(reading);
  await settle(() => expect(text()).toContain("Reading files: 70 of 100"));
  expect(text()).toContain("60 files can be searched, using 5.0 MB of text");
  expect(text()).toContain("8 files hold nothing to read");
  expect(text()).toContain("2 files could not be read");
});

test("paused and full say so, and everything read says it is up to date", async () => {
  show({ ...reading, paused: true });
  await settle(() => expect(text()).toContain("Paused: 30 files are waiting"));
  show({ ...reading, full: true });
  await settle(() => expect(text()).toContain("The limit for the text kept was reached: 30 files are waiting"));
  show({ ...reading, queued: 0 });
  await settle(() => expect(text()).toContain("Up to date"));
});

test("the limits are checked before they are saved, and pausing keeps them", async () => {
  show(reading);
  await settle(() => expect(document.querySelector("input[type=number]")).not.toBeNull());
  const [file, total] = [...document.querySelectorAll<HTMLInputElement>("input[type=number]")];
  expect([file.value, total.value]).toEqual(["25", "500"]);
  expect(button("Save").disabled).toBe(true);
  await type("input[type=number]", "0");
  expect(button("Save").disabled).toBe(true);
  await type("input[type=number]", "40");
  expect(button("Save").disabled).toBe(false);
  await act(async () => button("Save").click());
  await settle(() => expect(requests.saveContentSearch).toHaveBeenCalledWith({ enabled: true, paused: false, max_file_mb: 40, max_total_mb: 500 }));
  // Once the limits are saved, pausing keeps the saved ones
  await settle(() => expect(button("Save").disabled).toBe(true));
  await act(async () => toggle("Pause reading").click());
  await settle(() => expect(requests.saveContentSearch).toHaveBeenLastCalledWith({ enabled: true, paused: true, max_file_mb: 40, max_total_mb: 500 }));
});

test("indexing again asks first, and then deletes and reads again", async () => {
  show(reading);
  await settle(() => expect(button("Index again")).toBeDefined());
  await act(async () => button("Index again").click());
  expect(requests.rebuildContentSearch).not.toHaveBeenCalled();
  await settle(() => expect(text()).toContain("Index again?"));
  const confirm = [...document.querySelectorAll("button")].filter((b) => b.textContent?.trim() === "Index again").at(-1)!;
  await act(async () => confirm.click());
  await settle(() => expect(requests.rebuildContentSearch).toHaveBeenCalledTimes(1));
});

test("a snippet marks the words found, which are the odd pieces", () => {
  act(() => root.render(<Snippet pieces={["…agreed that the ", "renovation", " starts in May"]} />));
  const marks = [...host.querySelectorAll("mark")].map((m) => m.textContent);
  expect(marks).toEqual(["renovation"]);
  expect(host.textContent).toBe("…agreed that the renovation starts in May");
});
