// Control panel › Monitoring: turning /metrics on, making its token (shown once), and the list of addresses
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import type { MonitoringSettings } from "@/api";
import { MonitoringPage } from "@/admin/system/MonitoringPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/admin/SettingsFrame", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/admin/SettingsFrame")>()),
  SettingsFrame: ({ children }: { children: ReactNode }) => <main>{children}</main>,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const off: MonitoringSettings = { enabled: false, has_token: false, token_created_at: null, allow: [], path: "/metrics" };

let root: Root | null = null;
let host: HTMLElement;
let sent: Record<string, unknown>[];

async function mount(start: MonitoringSettings, answer?: (body: Record<string, unknown>) => unknown) {
  sent = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      if (url.endsWith("/api/admin/monitoring") && init?.method === "PUT") {
        const body = JSON.parse(String(init.body)) as Record<string, unknown>;
        sent.push(body);
        return json(answer?.(body) ?? { ...start, enabled: body.enabled, allow: body.allow });
      }
      if (url.endsWith("/api/admin/monitoring")) return json(start);
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
        <MemoryRouter>
          <MonitoringPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.textContent).toContain("Answer requests for the metrics"));
  });
}

const button = (name: string) => {
  const found = [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
  expect(found, `button ${name}`).toBeDefined();
  return found!;
};
const toggle = () => host.querySelector<HTMLElement>('[role="switch"]')!;
async function type(el: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  await act(async () => {
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const password = () => host.querySelector<HTMLInputElement>("#monitoring-me-password")!;

beforeEach(() => {
  vi.stubGlobal("location", { origin: "https://files.example.com", host: "files.example.com", protocol: "https:" });
});

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
});

describe("Monitoring", () => {
  test("shows the address and cannot be turned on before there is a token", async () => {
    await mount(off);
    expect(host.textContent).toContain("https://files.example.com/metrics");
    expect(host.textContent).toContain("There is no token yet.");
    expect(toggle().getAttribute("aria-checked")).toBe("false");
    expect(toggle().hasAttribute("data-disabled") || toggle().getAttribute("aria-disabled") === "true" || (toggle() as HTMLButtonElement).disabled).toBe(true);
    expect(button("Save").disabled).toBe(true);
    // The example for Prometheus names this site
    expect(host.textContent).toContain('targets: ["files.example.com"]');
    expect(host.textContent).toContain("Authorization: Bearer <token>");
  });

  test("a token is made after the administrator confirms who they are, and shown once", async () => {
    await mount(off, () => ({ ...off, enabled: false, has_token: true, token_created_at: 1_700_000_000, token: "tfm_secret-token-value" }));
    expect(button("Make a token").disabled).toBe(true);
    await type(password(), "my-password");
    await act(async () => button("Make a token").click());
    await act(async () => {
      await vi.waitFor(() => expect(sent).toHaveLength(1));
    });
    expect(sent[0]).toMatchObject({ new_token: true, enabled: false, my_password: "my-password" });
    expect(host.textContent).toContain("tfm_secret-token-value");
    expect(host.textContent).toContain("Copy it now");
    // What is kept afterwards (the page's cache) has no token in it
    expect(host.textContent).toContain("Make a new token");
  });

  test("with a token it can be turned on, and the addresses are sent as a list", async () => {
    await mount({ ...off, has_token: true, token_created_at: 1_700_000_000 });
    await act(async () => toggle().click());
    expect(toggle().getAttribute("aria-checked")).toBe("true");
    await type(host.querySelector<HTMLTextAreaElement>("#monitoring-allow")!, "203.0.113.7\n10.1.0.0/16, 198.51.100.1");
    await type(password(), "my-password");
    await act(async () => button("Save").click());
    await act(async () => {
      await vi.waitFor(() => expect(sent).toHaveLength(1));
    });
    expect(sent[0]).toMatchObject({ enabled: true, new_token: false, allow: ["203.0.113.7", "10.1.0.0/16", "198.51.100.1"] });
    expect(host.textContent).toContain("A token was made on");
  });

  test("nothing to save until something changes", async () => {
    await mount({ enabled: true, has_token: true, token_created_at: 1, allow: ["10.0.0.0/8"], path: "/metrics" });
    await type(password(), "my-password");
    expect(button("Save").disabled).toBe(true);
    await type(host.querySelector<HTMLTextAreaElement>("#monitoring-allow")!, "10.0.0.0/8\n");
    expect(button("Save").disabled).toBe(true);
    await type(host.querySelector<HTMLTextAreaElement>("#monitoring-allow")!, "10.0.0.0/8, 10.1.0.1");
    expect(button("Save").disabled).toBe(false);
  });
});
