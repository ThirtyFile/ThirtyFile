// Passkeys on the sign-in page: signing in with one, the second step after a password, and what a cancelled prompt does
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { DEFAULT_BRANDING } from "@/lib/branding";
import type { PasskeyAnswer, PasskeyRequestOptions } from "@/api";
import { getPasskey, PasskeyCancelled, passkeysSupported } from "@/lib/passkeys";
import { LoginPage } from "@/pages/LoginPage";

vi.mock("@/lib/passkeys", async (original) => ({
  ...(await original<typeof import("@/lib/passkeys")>()),
  passkeysSupported: vi.fn<() => boolean>(() => true),
  getPasskey: vi.fn<(o: PasskeyRequestOptions) => Promise<PasskeyAnswer>>(),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const OPTIONS: PasskeyRequestOptions = { challenge: "Y2hhbGxlbmdl", rp_id: "files.example.com", timeout: 120000, user_verification: "required", allow: [] };
const ANSWER: PasskeyAnswer = { credential_id: "Y3JlZA", client_data_json: "Y2xpZW50", authenticator_data: "YXV0aA", signature: "c2ln", user_handle: "dXNlcg" };
const ME = { id: 1, username: "amy", role: "user" };

let root: Root | null = null;
let host: HTMLElement;
let calls: { url: string; body: unknown }[];

type Pending = { two_factor: "code"; ticket: string; totp: boolean; passkey: boolean };

/** The server: what it says about passwords and passkeys, and how it answers the passkey and password requests */
function serve({
  passwords = "everyone",
  passkeys = true,
  providers = [] as { id: string; label: string }[],
  login = (): Response => json(ME),
  password = (): Response => json(ME),
}: { passwords?: string; passkeys?: boolean; providers?: { id: string; label: string }[]; login?: () => Response; password?: () => Response } = {}) {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input, init) => {
      const url = String(input);
      calls.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.endsWith("/api/auth/options")) return json({ password_reset: false, password_sign_in: passwords, passkeys });
      if (url.endsWith("/api/auth/sso/providers")) return json(providers);
      if (url.includes("/api/branding")) return json({ ...DEFAULT_BRANDING, login_lock: false });
      if (url.endsWith("/api/auth/passkey/options") || url.endsWith("/api/auth/passkey/second/options")) return json(OPTIONS);
      if (url.endsWith("/api/auth/passkey/login") || url.endsWith("/api/auth/passkey/second")) return login();
      if (url.endsWith("/api/auth/login")) return password();
      return json({}, 404);
    }),
  );
}

async function mount() {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/login"]}>
          <LoginPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.querySelector("h1")).not.toBeNull());
  });
}

const text = () => host.textContent ?? "";
const passkeyButton = () => [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => (b.getAttribute("aria-label") ?? b.textContent?.trim()) === "Sign in with a passkey");
const secondButton = () => [...host.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === "Use a passkey");
const codeField = () => host.querySelector<HTMLInputElement>('input[autocomplete="one-time-code"]');
const ready = () => act(async () => vi.waitFor(() => expect(passkeyButton()).toBeDefined()));

/** Types the account and password and presses the arrow: the password step of signing in */
async function signInWithPassword() {
  await act(async () => vi.waitFor(() => expect(host.querySelector('input[type="password"]')).not.toBeNull()));
  const set = (el: HTMLInputElement, value: string) => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  };
  await act(async () => {
    // (after a sign-in the page remembers the account, and asks only for the password)
    const account = host.querySelector<HTMLInputElement>('input[autocomplete="username"]:not(.sr-only)');
    if (account) set(account, "amy");
    set(host.querySelector<HTMLInputElement>('input[type="password"]')!, "pw");
  });
  await act(async () => host.querySelector<HTMLFormElement>("form")!.requestSubmit());
}

beforeEach(() => {
  window.__TF_BRANDING__ = { ...DEFAULT_BRANDING, login_lock: false };
  vi.stubGlobal("location", { origin: window.location.origin, assign: vi.fn<(url: string) => void>() });
  vi.mocked(passkeysSupported).mockReturnValue(true);
  vi.mocked(getPasskey).mockReset();
  vi.mocked(getPasskey).mockResolvedValue(ANSWER);
});

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
  window.__TF_BRANDING__ = undefined;
});

describe("signing in with a passkey", () => {
  test("asks the browser for one and sends its answer, with no user name or password", async () => {
    serve();
    await mount();
    await ready();
    await act(async () => passkeyButton()!.click());
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Welcome"));
    });
    expect(vi.mocked(getPasskey)).toHaveBeenCalledWith(OPTIONS);
    const sent = calls.find((c) => c.url.endsWith("/api/auth/passkey/login"));
    expect(sent?.body).toEqual(ANSWER);
    expect(calls.some((c) => c.url.endsWith("/api/auth/login"))).toBe(false);
  });

  test("closing the browser's prompt says nothing and nothing is sent", async () => {
    serve();
    vi.mocked(getPasskey).mockRejectedValue(new PasskeyCancelled());
    await mount();
    await ready();
    await act(async () => passkeyButton()!.click());
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(calls.some((c) => c.url.endsWith("/api/auth/passkey/login"))).toBe(false);
    expect(passkeyButton()!.disabled).toBe(false);
  });

  test("a passkey the server does not accept shows its message", async () => {
    serve({ login: () => json({ error: "The passkey was not accepted", code: "passkey_failed" }, 401) });
    await mount();
    await ready();
    await act(async () => passkeyButton()!.click());
    await act(async () => {
      await vi.waitFor(() => expect(host.querySelector('[role="alert"]')?.textContent).toContain("The passkey was not accepted"));
    });
  });

  test("is not offered when the site's address can't carry passkeys, or this browser has none", async () => {
    serve({ passkeys: false });
    await mount();
    await act(async () => vi.waitFor(() => expect(host.querySelector('input[type="password"]')).not.toBeNull()));
    expect(passkeyButton()).toBeUndefined();
    act(() => root?.unmount());
    host.remove();

    serve({ passkeys: true });
    vi.mocked(passkeysSupported).mockReturnValue(false);
    await mount();
    await act(async () => vi.waitFor(() => expect(host.querySelector('input[type="password"]')).not.toBeNull()));
    expect(passkeyButton()).toBeUndefined();
  });

  test("where passwords are off there is no passkey button either: the page is the providers alone", async () => {
    // (the server says passkeys are not offered, as it does for "nobody"; and "administrators" has no form for everyone)
    for (const passwords of ["nobody", "administrators"]) {
      serve({
        passwords,
        passkeys: passwords === "administrators",
        providers: [
          { id: "google", label: "Google" },
          { id: "github", label: "GitHub" },
        ],
      });
      await mount();
      await act(async () => vi.waitFor(() => expect(text()).toContain("Your administrator requires single sign-on.")));
      expect({ passwords, button: passkeyButton() }).toEqual({ passwords, button: undefined });
      expect(host.querySelectorAll("input:not(.sr-only)")).toHaveLength(0);
      act(() => root?.unmount());
      host.remove();
    }
  });

  test("at the administrators' address, where their form is, the passkey is offered with it", async () => {
    serve({ passwords: "administrators" });
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    await act(async () => {
      root!.render(
        <QueryClientProvider client={qc}>
          <MemoryRouter initialEntries={["/sign-in/administrator"]}>
            <LoginPage administrator />
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
    await ready();
    expect(host.querySelector('input[type="password"]')).not.toBeNull();
  });
});

describe("a passkey as the second step after a password", () => {
  const pending = (over: Partial<Pending>): Pending => ({ two_factor: "code", ticket: "TICKET", totp: true, passkey: true, ...over });

  test("beside the code, when the account has both", async () => {
    serve({ password: () => json(pending({})) });
    await mount();
    await signInWithPassword();
    await act(async () => vi.waitFor(() => expect(secondButton()).toBeDefined()));
    expect(codeField()).not.toBeNull();
    await act(async () => secondButton()!.click());
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Welcome"));
    });
    const options = calls.find((c) => c.url.endsWith("/api/auth/passkey/second/options"));
    expect(options?.body).toEqual({ ticket: "TICKET" });
    expect(calls.find((c) => c.url.endsWith("/api/auth/passkey/second"))?.body).toEqual({ ticket: "TICKET", ...ANSWER });
  });

  test("alone, when the account has no app: no field for a code", async () => {
    serve({ password: () => json(pending({ totp: false })) });
    await mount();
    await signInWithPassword();
    await act(async () => vi.waitFor(() => expect(secondButton()).toBeDefined()));
    expect(codeField()).toBeNull();
    expect(text()).toContain("Use your passkey to finish signing in.");
  });

  test("an account without passkeys sees only the code", async () => {
    serve({ password: () => json(pending({ passkey: false })) });
    await mount();
    await signInWithPassword();
    await act(async () => vi.waitFor(() => expect(codeField()).not.toBeNull()));
    expect(secondButton()).toBeUndefined();
  });

  test("a sign-in that took too long starts again from the password", async () => {
    serve({ password: () => json(pending({})), login: () => json({ error: "The sign-in has expired. Enter your password again.", code: "two_factor_expired" }, 401) });
    await mount();
    await signInWithPassword();
    await act(async () => vi.waitFor(() => expect(secondButton()).toBeDefined()));
    await act(async () => secondButton()!.click());
    await act(async () => {
      await vi.waitFor(() => expect(host.querySelector('[role="alert"]')?.textContent).toContain("expired"));
    });
    expect(secondButton()).toBeUndefined();
  });
});
