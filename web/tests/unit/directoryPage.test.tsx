// Control panel › Directory sign-in: the form, the kinds of directory to start from, saving (after saying who is asking),
// the test that says where it goes wrong, and the sync
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vite-plus/test";
import type { DirectoryInfo, DirectorySettings } from "@/api";
import { DirectoryPage } from "@/admin/users/DirectoryPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/admin/SettingsFrame", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/admin/SettingsFrame")>()),
  SettingsFrame: ({ children }: { children: ReactNode }) => <main>{children}</main>,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

const settings = (over: Partial<DirectorySettings> = {}): DirectorySettings => ({
  enabled: false,
  url: "",
  starttls: true,
  ca_pem: "",
  bind_dn: "",
  bind_password: "",
  user_base: "",
  user_filter: "",
  attr_id: "entryUUID",
  attr_username: "uid",
  attr_name: "cn",
  attr_email: "mail",
  require_group: "",
  group_mode: "none",
  group_base: "",
  group_filter: "",
  mappings: [],
  create_accounts: true,
  ...over,
});

let root: Root | null = null;
let host: HTMLElement;
let calls: { url: string; method: string; body: Record<string, unknown> | undefined }[];

async function mount(info: DirectoryInfo, answers: Record<string, () => Response> = {}) {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ url, method, body: init?.body ? (JSON.parse(String(init.body)) as Record<string, unknown>) : undefined });
      const custom = answers[`${method} ${url.replace(/^.*\/api/, "")}`];
      if (custom) return custom();
      if (url.endsWith("/api/admin/directory") && method === "PUT") return json({ ...info, settings: { ...(calls.at(-1)!.body as object), bind_password: "" }, has_bind_password: true });
      if (url.endsWith("/api/admin/directory")) return json(info);
      if (url.endsWith("/api/admin/groups")) return json([{ id: 5, name: "Staff", description: "", created_at: 0, source: "local", members: [] }]);
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
          <DirectoryPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.querySelector("#dir-url")).not.toBeNull());
  });
}

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
});

const field = (id: string) => host.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`#${id}`)!;
async function type(id: string, value: string) {
  await act(async () => {
    const el = field(id);
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
}
const button = (name: string) => [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
const text = () => host.textContent ?? "";

const info = (over: Partial<DirectoryInfo> = {}): DirectoryInfo => ({ settings: settings(), has_bind_password: false, last_sync: null, ...over });

describe("directory sign-in settings", () => {
  test("starts from the settings of a kind of directory", async () => {
    await mount(info());
    await type("dir-preset", "ad");
    expect(field("dir-attr-id").value).toBe("objectGUID");
    expect(field("dir-attr-username").value).toBe("sAMAccountName");
    expect(field("dir-user-filter").value).toContain("userPrincipalName={username}");
    expect(field("dir-group-mode").value).toBe("member_of");
    await type("dir-preset", "openldap");
    expect(field("dir-attr-id").value).toBe("entryUUID");
    expect(field("dir-group-mode").value).toBe("search");
    // A search shows the base and the filter with {dn}
    expect(field("dir-group-filter").value).toContain("{dn}");
  });

  test("StartTLS is asked for only with a plain address", async () => {
    await mount(info());
    expect(text()).not.toContain("Use StartTLS");
    await type("dir-url", "ldaps://dc.example.com");
    expect(text()).not.toContain("Use StartTLS");
    await type("dir-url", "ldap://dc.example.com");
    expect(text()).toContain("Use StartTLS");
  });

  test("a saved password is not shown, and a blank one is sent as blank to keep it", async () => {
    await mount(info({ settings: settings({ url: "ldaps://dc.example.com", bind_dn: "cn=reader" }), has_bind_password: true }));
    expect(field("dir-bind-password").value).toBe("");
    expect(text()).toContain("One is saved. Type a new one to replace it.");
  });

  test("saves what is typed after the administrator says who they are", async () => {
    await mount(info());
    expect(button("Save")!.disabled).toBe(true);
    await type("dir-url", "ldaps://dc.example.com");
    await type("dir-bind-dn", "cn=reader,dc=example,dc=com");
    await type("dir-bind-password", "reader-secret");
    // Not yet: the password of the administrator is asked for
    expect(button("Save")!.disabled).toBe(true);
    await type("directory-me-password", "my password");
    expect(button("Save")!.disabled).toBe(false);
    await act(async () => button("Save")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "PUT")).toBe(true));
    });
    const sent = calls.find((c) => c.method === "PUT")!.body!;
    expect(sent).toMatchObject({ url: "ldaps://dc.example.com", bind_dn: "cn=reader,dc=example,dc=com", bind_password: "reader-secret", my_password: "my password" });
    // The password isn't kept in the form afterwards
    await act(async () => {
      await vi.waitFor(() => expect(field("dir-bind-password").value).toBe(""));
    });
  });

  test("maps a group of the directory to a group here", async () => {
    await mount(info({ settings: settings({ group_mode: "member_of" }) }));
    await act(async () => button("Add a group")!.click());
    const dn = host.querySelector<HTMLInputElement>('input[aria-label="DN of the directory group"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(dn, "cn=staff,ou=groups,dc=example,dc=com");
      dn.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => {
      await vi.waitFor(() => expect(host.querySelector('select[aria-label="Group here"] option[value="5"]')).not.toBeNull());
    });
    const group = host.querySelector<HTMLSelectElement>('select[aria-label="Group here"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(group, "5");
      group.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await type("directory-me-password", "my password");
    await act(async () => button("Save")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.method === "PUT")).toBe(true));
    });
    expect(calls.find((c) => c.method === "PUT")!.body!.mappings).toEqual([{ dn: "cn=staff,ou=groups,dc=example,dc=com", group_id: 5 }]);
  });

  test("the test shows each step and where it stopped, with the exact reason", async () => {
    await mount(info({ settings: settings({ url: "ldaps://dc.example.com" }) }), {
      "POST /admin/directory/test": () =>
        json({
          steps: [
            { step: "Connect", ok: true, detail: "Connected to dc.example.com:636 over TLS" },
            { step: "Service account", ok: false, detail: "The directory refused it (invalid credentials)" },
          ],
        }),
    });
    await type("dir-test-user", "amy");
    await act(async () => button("Test")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(host.querySelectorAll('ol[aria-label="Test"] li')).toHaveLength(2));
    });
    expect(text()).toContain("Connected to dc.example.com:636 over TLS");
    expect(text()).toContain("invalid credentials");
    expect(host.querySelector('[aria-label="Worked"]')).not.toBeNull();
    expect(host.querySelector('[aria-label="Failed"]')).not.toBeNull();
    expect(calls.find((c) => c.url.endsWith("/test"))!.body).toMatchObject({ username: "amy", url: "ldaps://dc.example.com" });
  });

  test("syncs by hand, and says what the last run did", async () => {
    const report = { at: 1_800_000_000, checked: 12, turned_off: 1, turned_on: 0, groups_changed: 3, error: null };
    await mount(info({ settings: settings({ enabled: true, url: "ldaps://dc.example.com" }), last_sync: report }), {
      "POST /admin/directory/sync": () => json({ ...report, checked: 13 }),
    });
    expect(text()).toContain("12 people checked, 1 turned off, 0 turned on, 3 group memberships changed");
    await act(async () => button("Sync now")!.click());
    await act(async () => {
      await vi.waitFor(() => expect(calls.some((c) => c.url.endsWith("/api/admin/directory/sync"))).toBe(true));
    });
  });

  test("a sync that went wrong says why, and sync by hand needs the directory turned on", async () => {
    await mount(info({ last_sync: { at: 1_800_000_000, checked: 0, turned_off: 0, turned_on: 0, groups_changed: 0, error: "The directory can't be reached" } }));
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("The directory can't be reached");
    expect(button("Sync now")!.disabled).toBe(true);
  });
});
