// Administrator alerts: what is wrong now and what was sent, in words; the secret is typed once and kept when left blank
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";
import type { AlertsInfo } from "@/api";
import { AlertsPage } from "@/admin/system/AlertsPage";
import { alertText } from "@/lib/alerts";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const requests = vi.hoisted(() => ({
  alerts: vi.fn<typeof import("@/api").api.alerts>(),
  saveAlerts: vi.fn<typeof import("@/api").api.saveAlerts>(),
  testAlert: vi.fn<typeof import("@/api").api.testAlert>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});
// The Control panel frame needs a signed-in person; the page's own content is what is tested
vi.mock("@/admin/SettingsFrame", () => ({
  SettingsFrame: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Section: ({ title, children }: { title: string; children: React.ReactNode }) => (
    <section aria-label={title}>
      <h2>{title}</h2>
      {children}
    </section>
  ),
}));

const info: AlertsInfo = {
  email: true,
  webhook_url: "https://hooks.example.com/alerts",
  allow_private: false,
  usage: true,
  offline: true,
  data_disk: true,
  sign_ins: true,
  backups: true,
  certificate: true,
  sign_in_failures: 10,
  sign_in_minutes: 10,
  cooldown_minutes: 30,
  has_secret: true,
  email_ready: true,
  active: [
    { kind: "offline", params: { location: "NAS" }, since: 1_792_375_800, announced: true },
    { kind: "certificate", params: { days: 5 }, since: 1_792_375_900, announced: false },
  ],
  recent: [
    {
      id: 3,
      channel: "webhook",
      recipient: null,
      state: "resolved",
      kind: "offline",
      params: { location: "NAS" },
      created_at: 1_792_375_900,
      attempts: 1,
      done_at: 1_792_375_901,
      failed: false,
      error: null,
    },
    {
      id: 2,
      channel: "email",
      recipient: "admin",
      state: "firing",
      kind: "data_disk",
      params: { percent: 96 },
      created_at: 1_792_375_800,
      attempts: 2,
      done_at: null,
      failed: false,
      error: "can't connect",
    },
    {
      id: 1,
      channel: "webhook",
      recipient: null,
      state: "firing",
      kind: "usage_disk",
      params: { location: "NAS", percent: 91 },
      created_at: 1_792_375_700,
      attempts: 6,
      done_at: null,
      failed: true,
      error: "the webhook answered 500",
    },
  ],
};

let root: Root;
beforeEach(() => {
  Object.values(requests).forEach((r) => r.mockReset());
  requests.alerts.mockResolvedValue(info);
  requests.saveAlerts.mockImplementation(async (s) => ({ ...info, ...s }));
  requests.testAlert.mockResolvedValue({ ok: true });
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  act(() =>
    root.render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <AlertsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
});
afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});
const settle = (check: () => void) => act(async () => vi.waitFor(check));
const button = (name: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === name)!;
async function type(selector: string, value: string) {
  const el = document.querySelector<HTMLInputElement>(selector)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

test("each alert is said in words, with what it holds", () => {
  expect(alertText("usage_disk", { location: "NAS", percent: 93 })).toBe("The storage location “NAS” is 93% full");
  expect(alertText("data_disk", { percent: 97 })).toBe("The disk of the data folder is 97% full");
  expect(alertText("offline", { location: "NAS" })).toBe("The storage location “NAS” can't be reached");
  expect(alertText("sign_ins_ip", { ip: "203.0.113.9", count: 14, minutes: 10 })).toBe("14 failed sign-ins from 203.0.113.9 in the last 10 minutes");
  expect(alertText("sign_ins_user", { username: "amy", count: 14, minutes: 10 })).toBe("14 failed sign-ins for the account “amy” in the last 10 minutes");
  expect(alertText("certificate", { days: 6 })).toBe("The FTP certificate ends in 6 days");
  expect(alertText("backup", { name: "Nightly" })).toBe("The backup “Nightly” needs attention");
  expect(alertText("backup", { name: "Database", database: true })).toBe("The backup “Database” needs attention");
  expect(alertText("replica", { name: "Archive" })).toBe("The replicas “Archive” need attention");
});

test("what is wrong now and the latest messages are listed with how they went", async () => {
  await settle(() => expect(document.body.textContent).toContain("Wrong now"));
  const text = () => document.body.textContent ?? "";
  expect(text()).toContain("The storage location “NAS” can't be reached");
  expect(text()).toContain("Not told yet: waiting out the cooldown");
  expect(text()).toContain("Over: The storage location “NAS” can't be reached");
  expect(text()).toContain("Email to admin");
  expect(text()).toContain("Sent");
  expect(text()).toContain("Tried 2 times, to be tried again: can't connect");
  expect(text()).toContain("Given up after 6 tries: the webhook answered 500");
});

test("a secret is typed once and kept when left blank; a short one is refused", async () => {
  await settle(() => expect(document.querySelector("#alerts-secret")).not.toBeNull());
  const secret = document.querySelector<HTMLInputElement>("#alerts-secret")!;
  expect(secret.type).toBe("password");
  expect(secret.placeholder).toBe("Leave blank to keep the current secret");
  // Saved as it is: no secret is sent
  await act(async () => button("Save").click());
  await settle(() => expect(requests.saveAlerts).toHaveBeenCalledTimes(1));
  expect(requests.saveAlerts.mock.calls[0][0].secret).toBeUndefined();
  // A short one can't be saved
  await type("#alerts-secret", "too short");
  expect(document.body.textContent).toContain("The shared secret needs at least 16 characters");
  expect(button("Save").disabled).toBe(true);
  await type("#alerts-secret", "a secret of 24 characters");
  expect(button("Save").disabled).toBe(false);
  await act(async () => button("Save").click());
  await settle(() => expect(requests.saveAlerts).toHaveBeenCalledTimes(2));
  expect(requests.saveAlerts.mock.calls[1][0].secret).toBe("a secret of 24 characters");
});

test("the tests send what is saved: not while the address has been changed", async () => {
  await settle(() => expect(button("Send a test to the webhook")).toBeDefined());
  expect(button("Send a test to the webhook").disabled).toBe(false);
  await act(async () => button("Send a test to the webhook").click());
  await settle(() => expect(requests.testAlert).toHaveBeenCalledWith("webhook"));
  await act(async () => button("Send a test email to me").click());
  await settle(() => expect(requests.testAlert).toHaveBeenCalledWith("email"));
  await type("#alerts-url", "https://hooks.example.com/another");
  expect(button("Send a test to the webhook").disabled).toBe(true);
  expect(document.body.textContent).toContain("Save first: the test sends what is saved.");
});
