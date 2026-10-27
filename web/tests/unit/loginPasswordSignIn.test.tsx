// The sign-in page and who may use a password (Control panel › Single sign-on › Password sign-in): with passwords off for
// the person there is no user name or password field, no link to reset a password, and nothing that says a password exists
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { DEFAULT_BRANDING } from "@/lib/branding";
import type { PasswordSignIn } from "@/api";
import { LoginPage } from "@/pages/LoginPage";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const GOOGLE = { id: "google", label: "Google" };
const GITHUB = { id: "github", label: "GitHub" };

let root: Root | null = null;
let host: HTMLElement;
const assign = vi.fn<(url: string) => void>();

/** The server's answers: `options` is what /api/auth/options says (a promise, to hold it back), `providers` the buttons */
function serve(options: Promise<Response> | PasswordSignIn, providers: { id: string; label: string }[] = [GOOGLE], reset = true) {
  vi.stubGlobal(
    "fetch",
    vi.fn<typeof fetch>(async (input) => {
      const url = String(input);
      if (url.endsWith("/api/auth/options")) return typeof options === "string" ? json({ password_reset: reset && options === "everyone", password_sign_in: options }) : options;
      if (url.endsWith("/api/auth/sso/providers")) return json(providers);
      if (url.includes("/api/branding")) return json({ ...DEFAULT_BRANDING, login_lock: false });
      return json({}, 404);
    }),
  );
}

async function mount(path = "/login", administrator = false) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  await act(async () => {
    root!.render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[path]}>
          <LoginPage administrator={administrator} />
        </MemoryRouter>
      </QueryClientProvider>,
    );
  });
  await act(async () => {
    await vi.waitFor(() => expect(host.querySelector("h1")).not.toBeNull());
  });
}

const fields = () => host.querySelectorAll("input:not(.sr-only)").length;
const links = () => [...host.querySelectorAll("a")].map((a) => a.getAttribute("aria-label") ?? a.textContent?.trim());
const text = () => host.textContent ?? "";

beforeEach(() => {
  // (no lock screen in front of the form: the branding is there from the start, as the server puts it in the page)
  window.__TF_BRANDING__ = { ...DEFAULT_BRANDING, login_lock: false };
  assign.mockClear();
  vi.stubGlobal("location", { origin: window.location.origin, assign });
});

afterEach(() => {
  act(() => root?.unmount());
  host.remove();
  root = null;
  vi.unstubAllGlobals();
  window.__TF_BRANDING__ = undefined;
});

describe("the sign-in page with password sign-in", () => {
  test("open to everyone: the form, the reset link and the providers", async () => {
    serve("everyone");
    await mount();
    await act(async () => {
      await vi.waitFor(() => expect(links()).toEqual(expect.arrayContaining(["Forgot password?", "Sign in with Google"])));
    });
    expect(fields()).toBe(2);
    expect(host.querySelector('input[type="password"]')).not.toBeNull();
    expect(links()).toContain("Sign in with Google");
  });

  test("for administrators only: no form on the page everyone sees", async () => {
    serve("administrators");
    await mount("/login");
    await act(async () => {
      await vi.waitFor(() => expect(links()).toContain("Sign in with Google"));
    });
    expect(fields()).toBe(0);
    expect(host.querySelector('input[type="password"]')).toBeNull();
    expect(links()).not.toContain("Forgot password?");
    expect(text()).not.toMatch(/password|username/i);
    expect(text()).toContain("Your administrator requires single sign-on.");
  });

  test("for administrators only: the form is at the administrators' address", async () => {
    serve("administrators");
    await mount("/sign-in/administrator", true);
    await act(async () => {
      await vi.waitFor(() => expect(fields()).toBe(2));
    });
    expect(host.querySelector('input[type="password"]')).not.toBeNull();
  });

  test("nobody: no form at either address", async () => {
    serve("nobody", [GOOGLE, GITHUB]);
    for (const [path, administrator] of [
      ["/login", false],
      ["/sign-in/administrator", true],
    ] as const) {
      await mount(path, administrator);
      await act(async () => {
        await vi.waitFor(() => expect(links()).toContain("Sign in with GitHub"));
      });
      expect({ path, fields: fields() }).toEqual({ path, fields: 0 });
      expect(host.querySelector('input[type="password"]')).toBeNull();
      act(() => root?.unmount());
      host.remove();
    }
  });

  test("when everyone may, the administrators' address is the same page", async () => {
    serve("everyone");
    await mount("/sign-in/administrator", true);
    await act(async () => {
      await vi.waitFor(() => expect(fields()).toBe(2));
    });
  });

  test("the form is not drawn while the server hasn't said, so it never flashes on screen", async () => {
    let answer!: (r: Response) => void;
    serve(new Promise<Response>((done) => (answer = done)));
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
    expect(fields()).toBe(0);
    expect(host.querySelector('input[type="password"]')).toBeNull();
    // It answers: nobody. The form was never there
    answer(json({ password_reset: false, password_sign_in: "nobody" }));
    await act(async () => {
      await vi.waitFor(() => expect(links()).toContain("Sign in with Google"));
    });
    expect(fields()).toBe(0);
  });

  test("when the setting can't be read the form stays (the server refuses a password where it isn't allowed)", async () => {
    serve(Promise.resolve(json({ message: "boom" }, 500)));
    await mount();
    await act(async () => {
      await vi.waitFor(() => expect(fields()).toBe(2));
    });
  });

  test("with no password and no provider, it says so", async () => {
    serve("nobody", []);
    await mount();
    await act(async () => {
      await vi.waitFor(() => expect(text()).toContain("Ask your administrator"));
    });
    expect(fields()).toBe(0);
  });
});

describe("going straight to the only provider", () => {
  test("with no form and one provider, the page goes to it", async () => {
    serve("nobody", [GOOGLE]);
    await mount("/login?next=%2Ffiles%2Fabc");
    await act(async () => {
      await vi.waitFor(() => expect(assign).toHaveBeenCalled());
    });
    expect(assign).toHaveBeenCalledWith("/api/auth/sso/google/start?next=%2Ffiles%2Fabc");
  });

  test("not after signing out, or when the session ended, or with two providers, or with a form", async () => {
    for (const [path, mode, providers] of [
      ["/login?signed_out=1", "nobody", [GOOGLE]],
      ["/login?ended=1", "nobody", [GOOGLE]],
      ["/login", "nobody", [GOOGLE, GITHUB]],
      ["/login", "everyone", [GOOGLE]],
    ] as const) {
      serve(mode, [...providers]);
      await mount(path);
      // (both answers are in, and the page has had its turn to react to them)
      await act(async () => {
        await vi.waitFor(() =>
          expect(
            vi
              .mocked(fetch)
              .mock.calls.map(([u]) => String(u))
              .filter((u) => /options|providers/.test(u)),
          ).toHaveLength(2),
        );
        await new Promise((r) => setTimeout(r, 30));
      });
      expect(assign, `${path} ${mode} ${providers.length}`).not.toHaveBeenCalled();
      act(() => root?.unmount());
      host.remove();
    }
  });
});
