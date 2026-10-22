// Who may make share links: the question about links that would stop is never skipped, even when the count hasn't
// loaded or couldn't be read
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { api } from "@/api";
import { LinkMakersSetting } from "@/components/AccessDialog";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const requests = vi.hoisted(() => ({
  linksStopping: vi.fn<typeof import("@/api").api.linksStopping>(),
  updateDrive: vi.fn<typeof import("@/api").api.updateDrive>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

let root: Root;
let qc: QueryClient;

beforeEach(() => {
  Object.values(requests).forEach((request) => request.mockReset());
  qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(api.updateDrive).mockResolvedValue({} as never);
});
afterEach(async () => {
  await act(async () => {
    toast.dismiss();
  });
  await act(async () => root.unmount());
  qc.clear();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

async function mount() {
  await act(async () =>
    root.render(
      <QueryClientProvider client={qc}>
        <LinkMakersSetting driveId="space-1" value="editors" canChoose />
        <Toaster />
      </QueryClientProvider>,
    ),
  );
}
async function choose(value: string) {
  const select = document.querySelector("select")!;
  await act(async () => {
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await act(async () => new Promise((r) => setTimeout(r, 20)));
}

describe("who may make share links", () => {
  test("a count that can't be read changes nothing, rather than stop links without asking", async () => {
    vi.mocked(api.linksStopping).mockRejectedValue(new Error("offline"));
    await mount();
    await choose("nobody");
    expect(api.updateDrive).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Couldn't count the share links that would stop. Try again.");
  });

  test("the count is taken when the choice is made, and links that would stop are asked about first", async () => {
    // Nothing would stop when the dialog opened; two would by the time the choice is made
    vi.mocked(api.linksStopping).mockResolvedValueOnce({ managers: 0, nobody: 0 }).mockResolvedValue({ managers: 0, nobody: 2 });
    await mount();
    await choose("nobody");
    expect(document.body.textContent).toContain("Stop 2 share links?");
    expect(api.updateDrive).not.toHaveBeenCalled();
  });

  test("nothing that would stop: changed at once", async () => {
    vi.mocked(api.linksStopping).mockResolvedValue({ managers: 0, nobody: 0 });
    await mount();
    await choose("managers");
    expect(api.updateDrive).toHaveBeenCalledWith("space-1", { link_makers: "managers" });
  });
});
