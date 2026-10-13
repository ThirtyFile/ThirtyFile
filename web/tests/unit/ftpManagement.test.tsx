// Real query and identity hooks around FTP management: permissions, payloads, failures and one-time credentials.
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Toaster, toast } from "sonner";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api } from "@/api";
import type { FtpCreated, FtpFolder } from "@/api/ftp";
import { FtpFolderCard, FtpFolderDialog, FtpFolderMenu, FtpFolderSection } from "@/components/FtpFolders";
import { DropdownMenu, DropdownMenuContent, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

// Shared query options capture their API function at import time, so install boundary mocks before importing hooks.
const requests = vi.hoisted(() => ({
  ftpFolders: vi.fn<typeof import("@/api").api.ftpFolders>(),
  twoFactor: vi.fn<typeof import("@/api").api.twoFactor>(),
  createFtpFolder: vi.fn<typeof import("@/api").api.createFtpFolder>(),
  saveFtpFolder: vi.fn<typeof import("@/api").api.saveFtpFolder>(),
  rotateFtpPassword: vi.fn<typeof import("@/api").api.rotateFtpPassword>(),
  deleteFtpFolder: vi.fn<typeof import("@/api").api.deleteFtpFolder>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

const folder: FtpFolder = {
  id: "device-1",
  node_id: "incoming",
  name: "Office scanner",
  username: "ftp-device",
  creator_id: 1,
  creator: "amy",
  folder_name: "Incoming",
  enabled: true,
  allow_list: false,
  allow_read: false,
  expires_at: null,
  last_used_at: null,
  active: true,
};
const created: FtpCreated = { id: folder.id, node_id: folder.node_id, name: folder.name, username: folder.username, password: "new-device-password" };
let root: Root;
let qc: QueryClient;

beforeEach(() => {
  Object.values(requests).forEach((request) => request.mockReset());
  qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(api.ftpFolders).mockResolvedValue({ can_publish: true, folders: [] });
  vi.mocked(api.twoFactor).mockResolvedValue({ has_password: true, enabled: false, required: false, recovery_codes_left: 0 });
  vi.mocked(api.createFtpFolder).mockResolvedValue(created);
  vi.mocked(api.saveFtpFolder).mockResolvedValue(undefined);
  vi.mocked(api.rotateFtpPassword).mockResolvedValue(created);
  vi.mocked(api.deleteFtpFolder).mockResolvedValue(undefined);
});
afterEach(async () => {
  await act(async () => {
    toast.dismiss();
  });
  await act(async () => root.unmount());
  qc.clear();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});
async function mount(node: ReactNode) {
  await act(async () =>
    root.render(
      <QueryClientProvider client={qc}>
        {node}
        <Toaster />
      </QueryClientProvider>,
    ),
  );
}
const top = () => [...document.querySelectorAll<HTMLElement>('[role="dialog"]')].at(-1)!;
function button(name: string, scope: ParentNode = document.body) {
  const found = [...scope.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
  expect(found, `button ${name}`).toBeDefined();
  return found!;
}
function input(name: string, scope: ParentNode = top()) {
  const label = [...scope.querySelectorAll<HTMLLabelElement>("label")].find((l) => l.textContent?.trim() === name);
  expect(label, `field ${name}`).toBeDefined();
  return (label!.control ?? label!.querySelector("input")) as HTMLInputElement;
}
async function type(field: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const click = async (element: HTMLElement) => act(async () => element.click());
const settled = async (assert: () => void) => act(async () => vi.waitFor(assert));
async function createDialog() {
  await mount(<FtpFolderDialog nodeId="incoming" onClose={() => {}} />);
  await settled(() => expect(button("Make FTP receive folder…").disabled).toBe(false));
  await click(button("Make FTP receive folder…"));
  await settled(() => expect(input("Receive folder name")).toBeDefined());
}

describe("FTP receive folder management", () => {
  test("hides publishing and the Properties section when the account has no eligible publication", async () => {
    vi.mocked(api.ftpFolders).mockResolvedValue({ can_publish: false, folders: [] });
    await mount(<FtpFolderSection nodeId="private" />);
    await settled(() => expect(api.ftpFolders).toHaveBeenCalled());
    expect(document.body.textContent).not.toContain("Manage receive folders");
    await mount(<FtpFolderDialog nodeId="private" onClose={() => {}} />);
    await settled(() => expect(document.body.textContent).toContain("This folder can't be published as an FTP receive folder"));
    expect([...document.querySelectorAll("button")].some((b) => b.textContent === "Make FTP receive folder…")).toBe(false);
    expect(api.createFtpFolder).not.toHaveBeenCalled();
  });

  test("the folder menu offers publishing only after eligibility is known and invokes the selected action", async () => {
    const onPublish = vi.fn<() => void>();
    const menu = (
      <DropdownMenu open>
        <DropdownMenuTrigger>Options</DropdownMenuTrigger>
        <DropdownMenuContent>
          <FtpFolderMenu nodeId="incoming" onPublish={onPublish} />
        </DropdownMenuContent>
      </DropdownMenu>
    );
    await mount(menu);
    await settled(() => expect(document.querySelector('[role="menuitem"]')).not.toBeNull());
    await click(document.querySelector<HTMLElement>('[role="menuitem"]')!);
    expect(onPublish).toHaveBeenCalledTimes(1);
    vi.mocked(api.ftpFolders).mockResolvedValue({ can_publish: false, folders: [] });
    await act(async () => qc.invalidateQueries({ queryKey: ["ftp-folders"] }));
    await settled(() => expect(document.querySelector('[role="menuitem"]')).toBeNull());
  });

  test("Properties keeps existing stopped devices manageable even when new publishing is unavailable", async () => {
    vi.mocked(api.ftpFolders).mockResolvedValue({ can_publish: false, folders: [{ ...folder, active: false }] });
    await mount(<FtpFolderSection nodeId="incoming" />);
    await settled(() => expect(document.body.textContent).toContain("Office scanner · Stopped"));
    await click(button("Manage receive folders"));
    await settled(() => expect(top().textContent).toContain("Office scanner"));
    expect(top().textContent).toContain("No end date");
    expect(top().textContent).toContain("Never");
  });

  test("a failed listing is an error that can be retried rather than an empty folder list", async () => {
    vi.mocked(api.ftpFolders).mockRejectedValueOnce(new Error("The server didn't answer"));
    await mount(<FtpFolderDialog nodeId="incoming" onClose={() => {}} />);
    await settled(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("The server didn't answer"));
    expect(document.body.textContent).not.toContain("No FTP receive folders yet");
    await click(button("Try again"));
    await settled(() => expect(document.body.textContent).toContain("No FTP receive folders yet"));
    expect(api.ftpFolders).toHaveBeenCalledTimes(2);
  });

  test("creation defaults to upload-only and requires a name, password and enabled two-factor code", async () => {
    vi.mocked(api.twoFactor).mockResolvedValue({ has_password: true, enabled: true, required: false, recovery_codes_left: 0 });
    await createDialog();
    await settled(() => expect(input("Code from your app")).toBeDefined());
    expect(input("Enabled").checked).toBe(true);
    expect(input("Allow device to list files").checked).toBe(false);
    expect(input("Allow device to download files").checked).toBe(false);
    expect(button("Create", top()).disabled).toBe(true);
    await type(input("Receive folder name"), "  ");
    await type(input("Your current password"), "current-password");
    await type(input("Code from your app"), " 123456 ");
    expect(button("Create", top()).disabled).toBe(true);
    await click(button("Create", top()));
    expect(api.createFtpFolder).not.toHaveBeenCalled();
    await type(input("Receive folder name"), "Office scanner");
    await click(button("Create", top()));
    await settled(() =>
      expect(api.createFtpFolder).toHaveBeenCalledWith(
        "incoming",
        { name: "Office scanner", enabled: true, allow_list: false, allow_read: false, expires_at: null },
        { password: "current-password", code: "123456" },
      ),
    );
    await settled(() => expect(top().textContent).toContain("Save the device password now. It is shown only once."));
    expect(input("Device password").readOnly).toBe(true);
    expect(input("Device password").value).toBe(created.password);
    await click(button("Done", top()));
    await settled(() => expect(document.body.textContent).not.toContain("Save the device password now. It is shown only once."));
    expect([...document.querySelectorAll("input")].some((i) => i.value === created.password)).toBe(false);
  });

  test("copy failure is reported and credentials remain available for another copy attempt", async () => {
    const copy = vi.spyOn(navigator.clipboard, "writeText").mockRejectedValueOnce(new Error("Clipboard denied")).mockResolvedValue(undefined);
    await createDialog();
    await type(input("Receive folder name"), "Office scanner");
    await type(input("Your current password"), "current-password");
    await click(button("Create", top()));
    await settled(() => expect(top().textContent).toContain("Copy credentials"));
    await click(button("Copy credentials", top()));
    await settled(() => expect(document.body.textContent).toContain("Couldn't copy"));
    expect(input("Device password").value).toBe(created.password);
    await click(button("Copy credentials", top()));
    await settled(() => expect(document.body.textContent).toContain("Copied"));
    expect(copy).toHaveBeenLastCalledWith(`${created.username}\n${created.password}`);
    expect(api.createFtpFolder).toHaveBeenCalledTimes(1);
  });

  test("editing sends the chosen permissions and local end date without creating or rotating credentials", async () => {
    await mount(<FtpFolderCard folder={{ ...folder, expires_at: 1_800_000_000, last_used_at: 1_700_000_000 }} />);
    expect(document.body.textContent).not.toContain("Never");
    await click(button("Edit"));
    await settled(() => expect(input("Receive folder name").value).toBe(folder.name));
    await click(input("Enabled"));
    await click(input("Allow device to list files"));
    await click(input("Allow device to download files"));
    await type(input("End date"), "2027-10-15T09:45");
    await type(input("Your current password"), "current-password");
    await click(button("Save", top()));
    await settled(() =>
      expect(api.saveFtpFolder).toHaveBeenCalledWith(
        folder.id,
        expect.objectContaining({ enabled: false, allow_list: true, allow_read: true, expires_at: Math.floor(new Date("2027-10-15T09:45").getTime() / 1000) }),
        { password: "current-password", code: undefined },
      ),
    );
    await settled(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
    expect(api.createFtpFolder).not.toHaveBeenCalled();
    expect(api.rotateFtpPassword).not.toHaveBeenCalled();
  });

  test("passwordless rotation explains the recent sign-in requirement and retains a server refusal", async () => {
    vi.mocked(api.twoFactor).mockResolvedValue({ has_password: false, enabled: false, required: false, recovery_codes_left: 0 });
    vi.mocked(api.rotateFtpPassword).mockRejectedValue(new Error("Sign in again before changing credentials"));
    await mount(<FtpFolderCard folder={folder} />);
    await click(button("Rotate password"));
    await settled(() => expect(top().textContent).toContain("Sign in again before managing FTP credentials if your sign-in is older than 10 minutes."));
    expect(top().querySelector('input[type="password"]')).toBeNull();
    expect(button("Rotate password", top()).disabled).toBe(false);
    await click(button("Rotate password", top()));
    await settled(() => expect(top().querySelector('[role="alert"]')?.textContent).toContain("Sign in again before changing credentials"));
    expect(api.rotateFtpPassword).toHaveBeenCalledWith(folder.id, { password: undefined, code: undefined });
    expect(top().textContent).not.toContain("Device password");
    await click(button("Cancel", top()));
    await settled(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  });

  test("rotation displays the replacement credential once, clearing the confirmation fields", async () => {
    await mount(<FtpFolderCard folder={folder} />);
    await click(button("Rotate password"));
    await settled(() => expect(input("Your current password")).toBeDefined());
    expect(button("Rotate password", top()).disabled).toBe(true);
    await type(input("Your current password"), "current-password");
    await click(button("Rotate password", top()));
    await settled(() => expect(input("Device password").value).toBe(created.password));
    expect(top().querySelector('input[type="password"]')).toBeNull();
    expect(api.rotateFtpPassword).toHaveBeenCalledWith(folder.id, { password: "current-password", code: undefined });
    await click(button("Done", top()));
    await settled(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
    await click(button("Rotate password"));
    await settled(() => expect(input("Your current password").value).toBe(""));
    expect(button("Rotate password", top()).disabled).toBe(true);
  });

  test("deletion prevents repeat submissions while pending and preserves the error for retry", async () => {
    let refuse!: (reason: Error) => void;
    vi.mocked(api.deleteFtpFolder).mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          refuse = reject;
        }),
    );
    await mount(<FtpFolderCard folder={folder} />);
    await click(button("Delete"));
    await settled(() => expect(input("Your current password")).toBeDefined());
    expect(button("Delete", top()).disabled).toBe(true);
    await type(input("Your current password"), "current-password");
    await click(button("Delete", top()));
    await settled(() => expect(button("Saving…", top()).disabled).toBe(true));
    await click(button("Saving…", top()));
    expect(api.deleteFtpFolder).toHaveBeenCalledTimes(1);
    await act(async () => refuse(new Error("This receive folder could not be removed")));
    await settled(() => expect(top().querySelector('[role="alert"]')?.textContent).toContain("This receive folder could not be removed"));
    expect(button("Delete", top()).disabled).toBe(false);
    await click(button("Delete", top()));
    await settled(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
    expect(api.deleteFtpFolder).toHaveBeenLastCalledWith(folder.id, { password: "current-password", code: undefined });
    expect(api.deleteFtpFolder).toHaveBeenCalledTimes(2);
    expect(document.body.textContent).toContain(folder.folder_name);
  });
});
