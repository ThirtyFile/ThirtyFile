// Control panel › Single sign-on › Password sign-in: the choice, the administrators who keep the site reachable, and what is sent
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import type { PasswordSignIn, SsoAdmin, SsoProviderSettings, SsoSettings } from "@/api";
import { SsoPage } from "@/admin/users/SsoPage";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
vi.mock("@/admin/SettingsFrame", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/admin/SettingsFrame")>()),
  SettingsFrame: ({ children }: { children: ReactNode }) => <main>{children}</main>,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

const provider = (enabled: boolean): SsoProviderSettings => ({
  enabled,
  client_id: enabled ? "client" : "",
  has_secret: enabled,
  tenant: "",
  name: "",
  issuer: "",
  redirect_uri: "https://files.example.com/api/auth/sso/x/callback",
  provisioning: "link",
  allowed_domains: [],
  defaults: { guest: false, can_write: true, can_delete: true, can_share: true, quota_bytes: null },
  groups: [],
});

function settings(mode: PasswordSignIn, admins: SsoAdmin[] = [], on = true): SsoSettings {
  return {
    microsoft: provider(false),
    google: provider(on),
    github: provider(false),
    oidc: provider(false),
    allowed_domains: [],
    domain_rules: [],
    max_created_per_hour: 20,
    public_url_set: true,
    password_sign_in: mode,
    sso_admins: admins,
  };
}

let root: Root | null = null;
let host: HTMLElement;
let sent: Record<string, unknown>[];

async function mount(saved: SsoSettings) {
  sent = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      if (url.endsWith("/api/admin/sso") && init?.method === "PUT") {
        const body = JSON.parse(String(init.body)) as Record<string, unknown>;
        sent.push(body);
        return json({ ...saved, password_sign_in: body.password_sign_in });
      }
      if (url.endsWith("/api/admin/sso")) return json(saved);
      if (url.endsWith("/api/auth/2fa")) return json({ enabled: false, recovery_codes_left: 0, required: false, has_password: true });
      if (url.endsWith("/api/groups")) return json([]);
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
          <SsoPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.querySelector('input[name="password-sign-in"]')).not.toBeNull());
  });
}

const radio = (label: string) => {
  const found = [...host.querySelectorAll<HTMLLabelElement>("label")].find((l) => l.querySelector('input[name="password-sign-in"]') && l.textContent?.startsWith(label));
  expect(found, `choice ${label}`).toBeDefined();
  return found!.querySelector<HTMLInputElement>("input")!;
};
const choose = (label: string) => act(() => radio(label).click());
const text = () => host.textContent ?? "";

beforeEach(() => {
  vi.stubGlobal("location", { origin: "https://files.example.com", assign: vi.fn<(url: string) => void>() });
});

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
});

describe("Password sign-in", () => {
  test("shows what is saved, and nothing about administrators while everyone may", async () => {
    await mount(settings("everyone"));
    expect(radio("Everyone").checked).toBe(true);
    expect(radio("Administrators only").checked).toBe(false);
    expect(text()).not.toContain("Administrators who can sign in with a provider");
    expect(text()).not.toContain("password-sign-in everyone");
  });

  test("choosing a mode lists who can still sign in through a provider, the address and the way back", async () => {
    await mount(
      settings("everyone", [
        { id: 1, username: "kim", signed_in: true },
        { id: 2, username: "sam", signed_in: false },
      ]),
    );
    choose("Administrators only");
    expect(text()).toContain("Administrators who can sign in with a provider");
    const rows = [...host.querySelectorAll("ul[aria-label] li")].map((li) => li.textContent);
    expect(rows).toEqual(["kimhas signed in this way", "samhas not signed in this way yet"]);
    expect(text()).toContain("https://files.example.com/sign-in/administrator");
    expect(text()).toContain("thirtyfile password-sign-in everyone");
  });

  test("with no administrator able to, it says so, and says what to do first", async () => {
    await mount(settings("everyone", []));
    choose("Nobody");
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("No enabled administrator is linked to a sign-in provider that is on");
    expect(text()).not.toContain("https://files.example.com/sign-in/administrator");
  });

  test("with no provider on, it asks for one first", async () => {
    await mount(settings("everyone", [], false));
    choose("Administrators only");
    expect(host.querySelector('[role="alert"]')?.textContent).toContain("Turn on and set up a sign-in provider first");
  });

  test("for nobody, an administrator who never signed in through a provider is asked to do it first", async () => {
    await mount(settings("everyone", [{ id: 1, username: "kim", signed_in: false }]));
    choose("Nobody");
    expect(text()).toContain("Sign in through a provider once with your own account before choosing this");
  });

  test("the choice is saved with the settings", async () => {
    await mount(settings("everyone", [{ id: 1, username: "kim", signed_in: true }]));
    choose("Administrators only");
    const password = host.querySelector<HTMLInputElement>("#sso-me-password")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(password, "secret-password");
      password.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const save = [...host.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Save")!;
    await act(async () => save.click());
    await act(async () => {
      await vi.waitFor(() => expect(sent).toHaveLength(1));
    });
    expect(sent[0].password_sign_in).toBe("administrators");
    expect(sent[0].my_password).toBe("secret-password");
  });

  test("a saved mode other than everyone keeps the way back in view", async () => {
    await mount(settings("nobody", [{ id: 1, username: "kim", signed_in: true }]));
    expect(radio("Nobody").checked).toBe(true);
    expect(text()).toContain("thirtyfile password-sign-in everyone");
  });
});
