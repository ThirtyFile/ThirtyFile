// SSH keys under the account menu: the identity fields the other credential dialogs use, and a question before a key
// goes (programs signing in with it lose access)
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vite-plus/test";
import { api } from "@/api";
import { SftpKeysDialog } from "@/components/SftpKeysDialog";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const requests = vi.hoisted(() => ({
  sftpKeys: vi.fn<typeof import("@/api").api.sftpKeys>(),
  addSftpKey: vi.fn<typeof import("@/api").api.addSftpKey>(),
  removeSftpKey: vi.fn<typeof import("@/api").api.removeSftpKey>(),
  twoFactor: vi.fn<typeof import("@/api").api.twoFactor>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

let root: Root;
let qc: QueryClient;
const key = { id: "key-1", name: "Laptop", fingerprint: "SHA256:abc", created_at: 1, last_used_at: null };

beforeEach(() => {
  Object.values(requests).forEach((request) => request.mockReset());
  qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(api.sftpKeys).mockResolvedValue([key]);
  vi.mocked(api.twoFactor).mockResolvedValue({ has_password: true, enabled: true, required: false, recovery_codes_left: 0 });
  vi.mocked(api.removeSftpKey).mockResolvedValue(undefined);
});
afterEach(async () => {
  await act(async () => root.unmount());
  qc.clear();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

const wait = () => act(async () => new Promise((r) => setTimeout(r, 20)));
const button = (name: string) => [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name || b.getAttribute("aria-label") === name)!;

describe("SSH keys", () => {
  test("adding asks for the password and the code, as other credentials do", async () => {
    await act(async () =>
      root.render(
        <QueryClientProvider client={qc}>
          <SftpKeysDialog onClose={() => {}} />
        </QueryClientProvider>,
      ),
    );
    await wait();
    expect(document.querySelector("#sftp-password")).not.toBeNull();
    expect(document.querySelector("#sftp-code")).not.toBeNull();
    expect(button("Add SSH key").disabled).toBe(true);
  });

  test("removing a key asks first, and only then removes it", async () => {
    await act(async () =>
      root.render(
        <QueryClientProvider client={qc}>
          <SftpKeysDialog onClose={() => {}} />
        </QueryClientProvider>,
      ),
    );
    await wait();
    await act(async () => button("Remove SSH key Laptop").click());
    expect(api.removeSftpKey).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain('Remove the SSH key "Laptop"?');
    const confirm = [...document.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button, [role="dialog"] button')].filter((b) => b.textContent?.trim() === "Remove").at(-1)!;
    await act(async () => confirm.click());
    await wait();
    expect(api.removeSftpKey).toHaveBeenCalledWith("key-1");
  });
});
