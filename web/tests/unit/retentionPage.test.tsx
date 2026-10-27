// Control panel › Retention and holds: the holds and their files, releasing, placing one, and a space's retention period
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vite-plus/test";
import type { Hold } from "@/api";
import { RetentionPage } from "@/admin/system/RetentionPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/admin/SettingsFrame", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/admin/SettingsFrame")>()),
  SettingsFrame: ({ children }: { children: ReactNode }) => <main>{children}</main>,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const NOW = 1_800_000_000;

const hold = (over: Partial<Hold> = {}): Hold => ({
  id: "h1",
  scope: "node",
  label: "Legal/Cases/contract.txt",
  reason: "Case 2026-14",
  created_by_name: "admin",
  created_at: NOW,
  released_at: null,
  released_by_name: null,
  node_id: "n1",
  drive_id: null,
  user_id: null,
  present: true,
  ...over,
});

const space = (id: string, name: string, retention_days: number) => ({
  id,
  name,
  kind: "team",
  root_id: `root-${id}`,
  role: null,
  used_bytes: 0,
  quota_bytes: 0,
  owner_name: "",
  member_count: 0,
  disabled: false,
  location_id: null,
  location_name: "",
  offline: null,
  mode: "store",
  read_only: false,
  link_makers: "editors",
  retention_days,
});

let root: Root | null = null;
let host: HTMLElement;
let calls: { url: string; method: string; body: unknown }[];

async function mount(holds: Hold[] = [hold()]) {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.includes("/api/admin/holds") && url.includes("/items")) {
        return json({ items: [{ id: "f1", name: "contract.txt", space: "Legal", size: 2048, updated_at: NOW, owner: "amy", path: "Cases/contract.txt" }], more: true });
      }
      if (url.includes("/api/admin/holds") && method === "GET") return json({ holds, retention: [] });
      if (url.endsWith("/api/admin/holds") && method === "POST") return json(hold({ id: "h2" }));
      if (url.endsWith("/release")) return json({ ok: true });
      if (url.endsWith("/api/admin/drives")) return json([space("d1", "Legal", 30), space("d2", "Sales", 0)]);
      if (url.endsWith("/api/admin/users")) return json([{ id: 7, username: "amy" }]);
      if (/\/api\/drives\/d\d$/.test(url) && method === "PATCH") return json(space("d2", "Sales", 90));
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
        <MemoryRouter>
          <RetentionPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.textContent).not.toContain("Loading…"));
  });
}

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  document.body.innerHTML = "";
  root = null;
  vi.unstubAllGlobals();
});

const text = () => host.textContent ?? "";
const body = () => document.body;
const button = (name: string) => [...body().querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
async function type(selector: string, value: string) {
  await act(async () => {
    const el = body().querySelector<HTMLInputElement | HTMLTextAreaElement>(selector)!;
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("legal holds", () => {
  test("lists a hold with what it covers, why, who placed it, and its files", async () => {
    await mount();
    expect(text()).toContain("Legal/Cases/contract.txt");
    expect(text()).toContain("Case 2026-14");
    expect(text()).toContain("Folder or file");
    expect(text()).toContain("Placed by admin on");
    await act(async () => button("Files")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("1 file shown"));
    });
    expect(text()).toContain("Legal/Cases/contract.txt");
    expect(text()).toContain("More files are covered than are shown.");
    const link = host.querySelector<HTMLAnchorElement>('a[href="/api/admin/holds/h1/export"]');
    expect(link).not.toBeNull();
  });

  test("asks which state to show, and says when there is none", async () => {
    await mount([]);
    expect(text()).toContain("No holds.");
    const select = host.querySelector<HTMLSelectElement>('select[aria-label="Show"]')!;
    await act(async () => {
      select.value = "released";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.includes("state=released"))).toBe(true));
    });
  });

  test("releasing asks first, then releases", async () => {
    await mount();
    await act(async () => button("Release")!.click());
    expect(body().textContent).toContain('Release the hold on "Legal/Cases/contract.txt"?');
    await act(async () => [...body().querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find((b) => b.textContent?.trim() === "Release")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/admin/holds/h1/release") && c.method === "POST")).toBe(true));
    });
  });

  test("a released hold shows who released it and has no button to release", async () => {
    await mount([hold({ released_at: NOW + 60, released_by_name: "boss" })]);
    expect(text()).toContain("Released by boss on");
    expect(button("Release")).toBeUndefined();
  });

  test("a hold whose item is gone says so", async () => {
    await mount([hold({ present: false })]);
    expect(text()).toContain("Gone");
  });

  test("places a hold on a space or a person, with a reason", async () => {
    await mount();
    await act(async () => button("Place a hold")!.click());
    const place = () => [...body().querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find((b) => b.textContent?.trim() === "Place hold")!;
    expect(place().disabled).toBe(true);
    const target = body().querySelector<HTMLSelectElement>("#hold-target")!;
    await act(async () => {
      target.value = "d2";
      target.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await type("#hold-reason", "  Merger review  ");
    expect(place().disabled).toBe(false);
    await act(async () => place().click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url.endsWith("/api/admin/holds"))).toBe(true));
    });
    expect(calls.find((c) => c.method === "POST" && c.url.endsWith("/api/admin/holds"))!.body).toEqual({ scope: "space", drive_id: "d2", reason: "Merger review" });
  });
});

describe("retention periods", () => {
  test("lists the spaces with their periods, and saves a changed one", async () => {
    await mount();
    await act(async () => {
      await vi.waitFor(() => expect(body().querySelectorAll('ul[aria-label="Retention periods"] li')).toHaveLength(2));
    });
    const rows = [...body().querySelectorAll<HTMLInputElement>('ul[aria-label="Retention periods"] input')];
    expect(rows.map((r) => r.value)).toEqual(["30", "0"]);
    const save = (i: number) => [...body().querySelectorAll('ul[aria-label="Retention periods"] li')][i]!.querySelector<HTMLButtonElement>("button")!;
    expect(save(1).disabled).toBe(true);
    await type("#retention-d2", "90");
    expect(save(1).disabled).toBe(false);
    // Not a period the server takes
    await type("#retention-d2", "40000");
    expect(save(1).disabled).toBe(true);
    await type("#retention-d2", "90");
    await act(async () => save(1).click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    });
    const patch = calls.find((c) => c.method === "PATCH")!;
    expect(patch.url).toMatch(/\/api\/drives\/d2$/);
    expect(patch.body).toEqual({ retention_days: 90 });
  });
});
