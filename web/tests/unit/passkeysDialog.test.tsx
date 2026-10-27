// Account menu › Passkeys: the list, adding one (after the password), renaming, removing, and when they can't be used
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import type { PasskeyCreated, PasskeyCreationOptions, Passkeys } from "@/api";
import { createPasskey, PasskeyCancelled, passkeysSupported } from "@/lib/passkeys";
import { PasskeysDialog } from "@/components/PasskeysDialog";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/lib/passkeys", async (original) => ({
  ...(await original<typeof import("@/lib/passkeys")>()),
  passkeysSupported: vi.fn<() => boolean>(() => true),
  createPasskey: vi.fn<(o: PasskeyCreationOptions) => Promise<PasskeyCreated>>(),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const NOW = 1_800_000_000;
const CREATION: PasskeyCreationOptions = {
  challenge: "Y2hhbGxlbmdl",
  rp: { id: "files.example.com", name: "ThirtyFile" },
  user: { id: "dXNlcg", name: "amy", display_name: "amy" },
  timeout: 120000,
  algorithms: [-7, -8, -257],
  exclude: [],
};
const CREATED: PasskeyCreated = { client_data_json: "Y2xpZW50", attestation_object: "YXR0ZXN0", transports: ["internal"] };

const list = (over: Partial<Passkeys> = {}): Passkeys => ({
  passkeys: [
    { id: 1, name: "Work laptop", created_at: NOW - 86400, last_used_at: NOW - 3600, backup_eligible: false },
    { id: 2, name: "Phone", created_at: NOW - 40000, last_used_at: null, backup_eligible: true },
  ],
  available: true,
  has_password: true,
  max: 20,
  ...over,
});

let root: Root | null = null;
let host: HTMLElement;
let calls: { url: string; method: string; body: unknown }[];

async function mount(data: Passkeys, answers: Record<string, () => Response> = {}) {
  calls = [];
  let current = data;
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      const custom = answers[`${method} ${url.replace(/^.*\/api/, "")}`];
      if (custom) return custom();
      if (url.endsWith("/api/auth/2fa")) return json({ enabled: false, recovery_codes_left: 0, required: false, has_password: current.has_password });
      if (url.endsWith("/api/auth/passkeys") && method === "GET") return json(current);
      if (url.endsWith("/api/auth/passkeys/options")) return json(CREATION);
      if (url.endsWith("/api/auth/passkeys") && method === "POST") {
        current = { ...current, passkeys: [...current.passkeys, { id: 3, name: "New", created_at: NOW, last_used_at: null, backup_eligible: false }] };
        return json({ id: 3 });
      }
      if (/\/api\/auth\/passkeys\/\d+$/.test(url)) return json({ ok: true });
      return json({}, 404);
    }),
  );
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <PasskeysDialog onClose={() => undefined} />
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(document.body.textContent).toContain("Passkeys"));
    // (loaded: the list, the form or what says why there is none)
    await vi.waitFor(() => expect(text()).toMatch(/You have no passkeys|Add a passkey|Ask your administrator|can't use passkeys|Work laptop/));
  });
}

const body = () => document.body;
const text = () => body().textContent ?? "";
const button = (name: string) => [...body().querySelectorAll<HTMLButtonElement>("button")].find((b) => (b.getAttribute("aria-label") ?? b.textContent?.trim()) === name);
const type = async (selector: string, value: string) => {
  await act(async () => {
    const el = body().querySelector<HTMLInputElement>(selector)!;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
};

beforeEach(() => {
  vi.mocked(passkeysSupported).mockReturnValue(true);
  vi.mocked(createPasskey).mockReset();
  vi.mocked(createPasskey).mockResolvedValue(CREATED);
});

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
});

describe("the passkeys of an account", () => {
  test("lists them with when they were made and used, and marks the synced ones", async () => {
    await mount(list());
    const items = [...body().querySelectorAll('ul[aria-label="Your passkeys"] li')].map((li) => li.textContent);
    expect(items).toHaveLength(2);
    expect(items[0]).toContain("Work laptop");
    expect(items[0]).toContain("Last used");
    expect(items[1]).toContain("Phone");
    expect(items[1]).toContain("Never used");
    expect(items[1]).toContain("Synced");
    expect(items[0]).not.toContain("Synced");
  });

  test("says when there are none", async () => {
    await mount(list({ passkeys: [] }));
    expect(text()).toContain("You have no passkeys.");
  });

  test("adds one: the password first, then the browser makes it, then the server keeps it", async () => {
    await mount(list());
    expect(button("Add a passkey")!.disabled).toBe(true);
    await type("#pk-name", "Tablet");
    expect(button("Add a passkey")!.disabled).toBe(true);
    await type("#pk-password", "my password");
    expect(button("Add a passkey")!.disabled).toBe(false);
    await act(async () => button("Add a passkey")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "POST" && c.url.endsWith("/api/auth/passkeys"))).toBe(true));
    });
    expect(calls.find((c) => c.url.endsWith("/api/auth/passkeys/options"))?.body).toEqual({ password: "my password" });
    expect(vi.mocked(createPasskey)).toHaveBeenCalledWith(CREATION);
    expect(calls.find((c) => c.method === "POST" && c.url.endsWith("/api/auth/passkeys"))?.body).toEqual({ name: "Tablet", ...CREATED });
    // The list is read again, and the name and password are cleared
    await act(async () => {
      await vi.waitFor(() => expect(body().querySelectorAll('ul[aria-label="Your passkeys"] li')).toHaveLength(3));
    });
    expect(body().querySelector<HTMLInputElement>("#pk-name")!.value).toBe("");
    expect(body().querySelector<HTMLInputElement>("#pk-password")!.value).toBe("");
  });

  test("closing the browser's prompt adds nothing and says so", async () => {
    vi.mocked(createPasskey).mockRejectedValue(new PasskeyCancelled());
    await mount(list());
    await type("#pk-name", "Tablet");
    await type("#pk-password", "my password");
    await act(async () => button("Add a passkey")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("The passkey was not added. Try again."));
    });
    expect(calls.some((c) => c.method === "POST" && c.url.endsWith("/api/auth/passkeys"))).toBe(false);
  });

  test("a wrong password is the server's message, and nothing is made", async () => {
    await mount(list(), { "POST /auth/passkeys/options": () => json({ error: "Incorrect password" }, 400) });
    await type("#pk-name", "Tablet");
    await type("#pk-password", "wrong");
    await act(async () => button("Add a passkey")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Incorrect password"));
    });
    expect(vi.mocked(createPasskey)).not.toHaveBeenCalled();
  });

  test("renames one", async () => {
    await mount(list());
    await act(async () => button("Rename")!.click());
    await type('input[aria-label="Name"]', "Office laptop");
    await act(async () => button("Save")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "PATCH")).toBe(true));
    });
    const patch = calls.find((c) => c.method === "PATCH")!;
    expect(patch.url).toMatch(/\/api\/auth\/passkeys\/1$/);
    expect(patch.body).toEqual({ name: "Office laptop" });
  });

  test("removes one only after the password is entered", async () => {
    await mount(list());
    const remove = () => [...body().querySelectorAll<HTMLButtonElement>("button")].filter((b) => b.textContent?.trim() === "Remove")[0]!;
    expect(remove().disabled).toBe(true);
    await type("#pk-password", "my password");
    expect(remove().disabled).toBe(false);
    await act(async () => remove().click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "DELETE")).toBe(true));
    });
    const del = calls.find((c) => c.method === "DELETE")!;
    expect(del.url).toMatch(/\/api\/auth\/passkeys\/1$/);
    expect(del.body).toEqual({ password: "my password" });
  });

  test("an account at its limit can't add more", async () => {
    await mount(list({ max: 2 }));
    await type("#pk-name", "Tablet");
    await type("#pk-password", "my password");
    expect(button("Add a passkey")!.disabled).toBe(true);
    expect(text()).toContain("An account can have 2 passkeys at most.");
  });

  test("an account without a password isn't asked for one", async () => {
    await mount(list({ has_password: false }));
    expect(body().querySelector("#pk-password")).toBeNull();
    await type("#pk-name", "Tablet");
    expect(button("Add a passkey")!.disabled).toBe(false);
  });
});

describe("when passkeys can't be used", () => {
  test("a browser without them says so, offers no way to add one, and still lets one be removed", async () => {
    vi.mocked(passkeysSupported).mockReturnValue(false);
    await mount(list());
    expect(text()).toContain("This browser can't use passkeys here.");
    expect(button("Add a passkey")).toBeUndefined();
    expect(body().querySelector("#pk-password")).not.toBeNull();
  });

  test("a site address that can't carry them tells the person to ask the administrator", async () => {
    await mount(list({ available: false, passkeys: [] }));
    expect(text()).toContain("Ask your administrator to set it in Control panel › General.");
    expect(button("Add a passkey")).toBeUndefined();
  });
});
