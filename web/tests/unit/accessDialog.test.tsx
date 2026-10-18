// Share with…: a row's own changes are separate from adding someone, and restricting a folder asks with the grants
// that stop as they are when it is chosen
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AccessInfo, Grant, Me, Node } from "@/api";
import { AccessDialog } from "@/components/AccessDialog";
import { MeContext } from "@/lib/session";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const requests = vi.hoisted(() => ({
  access: vi.fn<typeof import("@/api").api.access>(),
  grant: vi.fn<typeof import("@/api").api.grant>(),
  directory: vi.fn<typeof import("@/api").api.directory>(),
  restrictionPreview: vi.fn<typeof import("@/api").api.restrictionPreview>(),
  setRestriction: vi.fn<typeof import("@/api").api.setRestriction>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

const folder = { id: "folder-1", parent_id: "root", kind: "folder", name: "Projects", restricted: false } as Node;
const ben: Grant = {
  guest: false,
  id: 7,
  node_id: folder.id,
  principal_type: "user",
  principal_id: 12,
  principal_name: "ben",
  role: "viewer",
  expires_at: null,
  granted_by_name: "amy",
  created_at: 0,
  inherited_from: null,
};
const info: AccessInfo = {
  share_own_groups: false,
  guest_max_days: 90,
  node: folder,
  drive: { id: "drive-1", name: "Company", kind: "company", root_id: "root", link_makers: "editors" },
  is_drive_root: false,
  my_role: "manager",
  can_manage: true,
  can_restrict: false,
  direct: [ben],
  inherited: [],
};

let root: Root;

beforeEach(() => {
  Object.values(requests).forEach((request) => request.mockReset());
  Element.prototype.scrollIntoView = () => {};
  requests.grant.mockResolvedValue({} as never);
  requests.directory.mockResolvedValue([{ guest: false, principal_type: "user", principal_id: 13, name: "carol", detail: "User" }]);
  requests.setRestriction.mockResolvedValue({} as never);
});
afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});

function mount(given: AccessInfo = info) {
  requests.access.mockResolvedValue(given);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  act(() =>
    root.render(
      <QueryClientProvider client={qc}>
        <MeContext value={{ id: 1, role: "user", can_share: true } as Me}>
          <AccessDialog nodeId={folder.id} onClose={() => {}} />
        </MeContext>
      </QueryClientProvider>,
    ),
  );
}

const settle = (check: () => void) => act(async () => vi.waitFor(check));

const button = (name: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === name);

test("changing a row's role leaves the person being added picked, and each row's end date says whose it is", async () => {
  mount();
  await settle(() => expect(document.querySelector('input[aria-label="Access end date for ben"]')).not.toBeNull());
  const box = document.querySelector<HTMLInputElement>('input[role="combobox"]')!;
  await act(async () => {
    box.focus();
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(box, "car");
    box.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => new Promise((r) => setTimeout(r, 300)));
  await settle(() => expect(document.querySelector('[role="option"]')?.textContent).toContain("carol"));
  await act(async () => document.querySelector<HTMLElement>('[role="option"]')!.click());
  expect(document.querySelector('[aria-label="Choose again"]')).not.toBeNull();

  // The row's own role: the row's request, and carol stays picked
  const selects = [...document.querySelectorAll<HTMLSelectElement>('select[aria-label="Role"]')];
  const row = selects.find((s) => s.value === "viewer" && s.closest("form") === null)!;
  await act(async () => {
    row.value = "editor";
    row.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle(() => expect(requests.grant).toHaveBeenCalledExactlyOnceWith(folder.id, { principal_type: "user", principal_id: 12, role: "editor", expires_at: null }));
  await act(async () => new Promise((r) => setTimeout(r, 50)));
  expect(document.querySelector('[aria-label="Choose again"]')).not.toBeNull();
  expect(document.body.textContent).toContain("carol");
});

test("restricting a folder asks with the inherited grants as they are then, and changes nothing when they can't be read", async () => {
  requests.restrictionPreview
    .mockResolvedValueOnce({ restricted: false, inherited: [] })
    .mockRejectedValueOnce(new Error("Couldn't load"))
    .mockResolvedValue({ restricted: false, inherited: [{ principal_type: "group", principal_id: 3, name: "Editor", role: "viewer" }] });
  mount({ ...info, can_restrict: true });
  await settle(() => expect(button("Restrict folder")?.disabled).toBe(false));
  // A list that can't be read: no question, nothing changed
  await act(async () => button("Restrict folder")!.click());
  await settle(() => expect(document.body.textContent).toContain("Couldn't load"));
  expect(document.querySelector('[role="alertdialog"]')).toBeNull();
  // Read again when chosen: the group added since the dialog opened is named, by its own name
  await act(async () => button("Restrict folder")!.click());
  await settle(() => expect(document.body.textContent).toContain("These inherited grants will stop: Editor (Viewer)."));
  await act(async () => button("Cancel")!.click());
  expect(requests.setRestriction).not.toHaveBeenCalled();
  await act(async () => button("Restrict folder")!.click());
  await settle(() => expect(button("Change")).toBeDefined());
  await act(async () => button("Change")!.click());
  await settle(() => expect(requests.setRestriction).toHaveBeenCalledExactlyOnceWith(folder.id, true, false));
});
