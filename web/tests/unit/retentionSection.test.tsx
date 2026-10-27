// What keeps an item from being deleted for good: the text for it, the report of a deletion that left items in the
// trash, and the Retention section of the details pane (managers see it, administrators place and release holds)
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { toast } from "sonner";
import type { Hold, NodeRetention } from "@/api";
import { RetentionSection } from "@/components/RetentionSection";
import { followJob } from "@/lib/jobs";
import { keptOf, keptReason, keptText } from "@/lib/kept";

vi.mock("sonner", () => ({
  toast: {
    error: vi.fn<(m: string) => void>(),
    success: vi.fn<(m: string) => void>(),
    warning: vi.fn<(m: string, o?: unknown) => void>(),
    loading: vi.fn<(m: string) => void>(),
    dismiss: vi.fn<() => void>(),
  },
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const NOW = 1_800_000_000;

const hold = (over: Partial<Hold> = {}): Hold => ({
  id: "h1",
  scope: "node",
  label: "Legal/Cases",
  reason: "Audit 2026",
  created_by_name: "admin",
  created_at: NOW,
  released_at: null,
  released_by_name: null,
  node_id: "folder",
  drive_id: null,
  user_id: null,
  present: true,
  ...over,
});

describe("what keeps an item", () => {
  test("says a hold, or the date a retention period runs until", () => {
    expect(keptText({ hold: "Case 12", until: null })).toBe("On legal hold");
    expect(keptText({ hold: null, until: NOW })).toMatch(/^Held until /);
    expect(keptText({ hold: null, until: null })).toBe("");
    expect(keptReason({ hold: "Case 12", until: NOW })).toBe("On legal hold: Case 12");
    expect(keptReason({ hold: null, until: NOW })).toMatch(/^Held until .*period after their last change$/);
  });

  test("reads the report of a deletion", () => {
    expect(keptOf({ result: undefined })).toEqual([]);
    expect(keptOf({ result: { deleted: 2 } })).toEqual([]);
    expect(keptOf({ result: { kept: "nothing" } })).toEqual([]);
    const kept = [{ id: "a", name: "a.txt", hold: "Case 12", until: null }];
    expect(keptOf({ result: { kept } })).toEqual(kept);
  });

  test("a deletion that left items in the trash says so, and says it is done when it left none", async () => {
    vi.mocked(toast.warning).mockClear();
    vi.mocked(toast.success).mockClear();
    const job = (result: unknown) => ({ id: "j", kind: "delete" as const, state: "done" as const, done: 1, total: 1, error: null, node_id: null, name: null, result });
    const after = vi.fn<() => void>();
    await followJob(job({ kept: [{ id: "a", name: "held.txt", hold: "Case 12", until: null }] }), "Permanently deleted", after);
    expect(toast.success).not.toHaveBeenCalled();
    const [title, options] = vi.mocked(toast.warning).mock.calls[0]!;
    expect(title).toBe("1 item is held and stays in the trash");
    expect((options as { description: string }).description).toBe("held.txt: On legal hold");
    await followJob(job({ deleted: 2 }), "Permanently deleted", after);
    expect(toast.success).toHaveBeenCalledWith("Permanently deleted");
    expect(after).toHaveBeenCalledTimes(2);
  });
});

let root: Root | null = null;
let host: HTMLElement;
let calls: { url: string; method: string; body: unknown }[];

async function mount(answer: NodeRetention | number) {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.endsWith("/retention")) return typeof answer === "number" ? json({ error: "Only managers of this item can see what keeps it" }, answer) : json(answer);
      if (url.endsWith("/api/admin/holds")) return json(hold({ id: "h2" }));
      if (url.endsWith("/release")) return json({ ok: true });
      if (url.endsWith("/api/auth/2fa")) return json({ enabled: false, recovery_codes_left: 0, required: false, has_password: true });
      return json({}, 404);
    }),
  );
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <RetentionSection nodeId="n1" name="contract.txt" ready />
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/retention"))).toBe(true));
  });
}

const text = () => host.textContent ?? "";
const button = (name: string) => [...document.body.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);

beforeEach(() => {
  vi.mocked(toast.success).mockClear();
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  document.body.innerHTML = "";
  root = null;
  vi.unstubAllGlobals();
});

describe("Details pane › Retention", () => {
  test("shows the period of the space, until when this item is kept, and the holds on it", async () => {
    await mount({ retention_days: 30, kept_until: NOW, holds: [hold()], can_place: false });
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Retention"));
    });
    expect(text()).toContain("Files in this space are kept for 30 days after their last change.");
    expect(text()).toContain("This one is kept until");
    expect(text()).toContain("On legal hold");
    expect(text()).toContain("Audit 2026");
    expect(text()).toContain("Placed on Legal/Cases");
    // A manager can't place or release
    expect(button("Place a legal hold")).toBeUndefined();
    expect(button("Release")).toBeUndefined();
  });

  test("says when nothing keeps it, to an administrator who can place a hold", async () => {
    await mount({ retention_days: 0, kept_until: null, holds: [], can_place: true });
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Nothing keeps this from being deleted for good."));
    });
    expect(button("Place a legal hold")).toBeDefined();
  });

  test("shows nothing to a manager when nothing keeps the item, and nothing to someone who may not see it", async () => {
    await mount({ retention_days: 0, kept_until: null, holds: [], can_place: false });
    expect(text()).toBe("");
    act(() => root?.unmount());
    host.remove();
    await mount(403);
    expect(text()).toBe("");
  });

  test("an administrator places a hold with a reason", async () => {
    await mount({ retention_days: 0, kept_until: null, holds: [], can_place: true });
    await act(async () => {
      await vi.waitFor(() => expect(button("Place a legal hold")).toBeDefined());
    });
    await act(async () => button("Place a legal hold")!.click());
    expect(document.body.textContent).toContain('Place a legal hold on "contract.txt"?');
    const confirm = () => [...document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find((b) => b.textContent?.trim() === "Place hold")!;
    expect(confirm().disabled).toBe(true);
    await act(async () => {
      const el = document.body.querySelector<HTMLTextAreaElement>("#hold-reason")!;
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(el, "Case 12");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => confirm().click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url.endsWith("/api/admin/holds"))).toBe(true));
    });
    expect(calls.find((c) => c.method === "POST")!.body).toEqual({ scope: "node", node_id: "n1", reason: "Case 12" });
  });

  test("an administrator releases a hold after confirming", async () => {
    await mount({ retention_days: 0, kept_until: null, holds: [hold()], can_place: true });
    await act(async () => {
      await vi.waitFor(() => expect(button("Release")).toBeDefined());
    });
    await act(async () => button("Release")!.click());
    expect(document.body.textContent).toContain('Release the hold on "Legal/Cases"?');
    const confirm = [...document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find((b) => b.textContent?.trim() === "Release")!;
    await act(async () => confirm.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/admin/holds/h1/release"))).toBe(true));
    });
  });
});
