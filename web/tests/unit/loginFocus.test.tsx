// The cursor goes into the username field when the form shows, even if the server says who may use a password after the page
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, describe, expect, test, vi } from "vite-plus/test";
import { DEFAULT_BRANDING } from "@/lib/branding";
import { LoginPage } from "@/pages/LoginPage";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });

let root: Root | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

describe("sign-in focus", () => {
  test("a form that shows after the options arrive gets the cursor", async () => {
    let answer!: () => void;
    const options = new Promise<void>((resolve) => (answer = resolve));
    vi.stubGlobal(
      "fetch",
      vi.fn<typeof fetch>(async (input) => {
        const url = String(input);
        if (url.endsWith("/api/auth/options")) {
          await options;
          return json({ password_reset: false, password_sign_in: "everyone", passkeys: false });
        }
        if (url.endsWith("/api/auth/sso/providers")) return json([]);
        if (url.includes("/api/branding")) return json({ ...DEFAULT_BRANDING, login_lock: false });
        return json({});
      }),
    );
    const host = document.createElement("div");
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
    // Unlocked while the options are still on their way
    await act(async () => {
      host.querySelector<HTMLElement>('[aria-label="Click or press any key to sign in"]')?.click();
    });
    expect(host.querySelector('input[autocomplete="username"]')).toBeNull();
    await act(async () => {
      answer();
      await vi.waitFor(() => expect(host.querySelector('input[autocomplete="username"]')).not.toBeNull());
    });
    await vi.waitFor(() => expect(document.activeElement).toBe(host.querySelector('input[autocomplete="username"]')));
  });
});
