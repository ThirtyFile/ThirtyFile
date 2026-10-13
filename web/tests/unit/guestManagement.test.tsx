import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { api, ApiError, type GuestConversionPreview, type UserRow } from "@/api";
import { keys } from "@/api/queryKeys";
import { UserDialog } from "@/admin/users/UserDialog";
import { GrantExpiry } from "@/components/GrantExpiry";
import { toast } from "sonner";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

// Keep the account form and its query/mutation state real; the dialog shell does not need a focus trap here.
vi.mock("@/components/ui/dialog", () => {
  const Part = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return { Dialog: Part, DialogContent: Part, DialogHeader: Part, DialogTitle: Part, DialogFooter: Part, DialogDescription: Part };
});
vi.mock("@/lib/session", () => ({ useMe: () => ({ min_password_length: 12 }) }));
vi.mock("@/components/LocationSelect", () => ({
  useLocationName: () => () => "Local disk",
  useDefaultLocationId: () => "local-disk",
  LocationSelect: (props: { value: string; onChange(value: string): void; "aria-label": string }) => (
    <select aria-label={props["aria-label"]} value={props.value} onChange={(e) => props.onChange(e.target.value)}>
      <option value="">Default location</option>
      <option value="local-disk">Local disk</option>
    </select>
  ),
}));
vi.mock("sonner", () => ({ toast: { success: vi.fn<(message: string) => void>(), warning: vi.fn<(message: string) => void>() } }));

const account: UserRow = {
  id: 42,
  username: "amy",
  display_name: "Amy",
  role: "user",
  guest: false,
  can_write: true,
  can_delete: true,
  can_share: true,
  quota_bytes: 0,
  disabled: false,
  created_at: 1,
  last_login_at: null,
  sso: "",
  two_factor: false,
  source: "password",
  sso_email: "",
  used_bytes: 2048,
  personal_space: true,
  personal_location: "local-disk",
  personal_pending: null,
  files_folder: "Files of Amy",
};
const preview: GuestConversionPreview = {
  revision: "review-1",
  max_days: 30,
  personal_files: 3,
  personal_bytes: 2048,
  personal_space: true,
  owned_spaces: [],
  unsafe_groups: [],
  changed_grants: [{ id: 7, item_name: "Contracts", old_role: "manager", old_expires_at: null, role: "editor", expires_at: 1792000000 }],
  share_links: 2,
  s3_buckets: 1,
  ftp_folders: 4,
};

let root: Root;
let container: HTMLDivElement;
let qc: QueryClient;
const close = vi.fn<() => void>();
const personal = vi.fn<(what: "add" | "remove", user: UserRow) => void>();
const reviewKey = ["guest-conversion", account.id];

beforeEach(() => {
  vi.clearAllMocks();
  qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity }, mutations: { retry: false } } });
  qc.setQueryData(keys.twoFactor(), { has_password: true, enabled: false });
  qc.setQueryData(keys.system(), { default_user_quota: 2 * 1024 ** 3, personal_spaces: true, personal_location: "local-disk" });
  qc.setQueryData(reviewKey, preview);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  qc.clear();
  container.remove();
  vi.restoreAllMocks();
});

const show = async (node: ReactNode) => act(async () => root.render(<QueryClientProvider client={qc}>{node}</QueryClientProvider>));
const edit = (user: UserRow | null = account, self = false) => show(<UserDialog user={user} self={self} onClose={close} onPersonal={personal} />);
const button = (name: string) => {
  const found = [...container.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === name);
  if (!found) throw new Error(`Missing button: ${name}`);
  return found;
};
const click = async (target: HTMLElement) => act(async () => target.click());
const input = async (selector: string, text: string) => {
  const element = container.querySelector<HTMLInputElement>(selector);
  if (!element) throw new Error(`Missing input: ${selector}`);
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(element, text);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
};
const checkbox = (label: string) => {
  const found = [...container.querySelectorAll("label")].find((l) => l.textContent?.trim() === label)?.querySelector<HTMLElement>('[role="checkbox"]');
  if (!found) throw new Error(`Missing checkbox: ${label}`);
  return found;
};
const confirmReview = () => click(checkbox("I have reviewed and agree to these guest account changes").closest("label")!.querySelector<HTMLInputElement>('input[type="checkbox"]')!);
const settled = async (assert: () => void) => act(async () => vi.waitFor(assert));

describe("guest account management", () => {
  test("conversion requires the reviewed revision and the administrator's identity before submitting", async () => {
    const update = vi.spyOn(api, "updateUser").mockResolvedValue({ ...account, guest: true, can_share: false });
    qc.setQueryData(keys.twoFactor(), { has_password: true, enabled: true });
    await edit();
    await click(button("Guest"));
    expect(container.textContent).toContain("Personal files kept: 3");
    expect(container.textContent).toContain("Share links: 2 · S3 buckets: 1 · FTP receive folders: 4");
    expect(container.textContent).toContain("Contracts · Manager → Editor");
    expect(container.textContent).toContain('"My files" is hidden');
    expect(button("Save").disabled).toBe(true);
    expect(checkbox("Share").getAttribute("aria-disabled")).toBe("true");
    expect(checkbox("Share").getAttribute("aria-checked")).toBe("false");
    await confirmReview();
    expect(button("Save").disabled).toBe(true);
    await input("#u-me-password", "example-admin-password");
    expect(button("Save").disabled).toBe(true);
    await input("#u-me-code", " 123456 ");
    expect(button("Save").disabled).toBe(false);
    await click(button("Save"));
    await settled(() => expect(close).toHaveBeenCalledTimes(1));
    expect(update).toHaveBeenCalledWith(
      account.id,
      expect.objectContaining({ guest: true, role: "user", confirm_guest: true, guest_revision: "review-1", my_password: "example-admin-password", my_code: "123456" }),
    );
    expect(toast.success).toHaveBeenCalledWith("User updated");
  });

  test.each([
    { reason: "owned spaces", change: { owned_spaces: ["Finance"] }, warning: "Transfer these spaces to another owner first:", item: "Finance" },
    {
      reason: "unsafe group access",
      change: { unsafe_groups: [{ group_id: 3, group_name: "Reviewers", grant_id: 9, item_name: "Reports", role: "manager" as const, expires_at: null }] },
      warning: "Fix these group grants before converting.",
      item: "Reviewers → Reports · Manager",
    },
  ])("$reason cannot be acknowledged away to convert an account", async ({ change, warning, item }) => {
    const update = vi.spyOn(api, "updateUser");
    qc.setQueryData(reviewKey, { ...preview, ...change });
    await edit();
    await click(button("Guest"));
    await input("#u-me-password", "example-admin-password");
    expect(container.textContent).toContain(warning);
    expect(container.textContent).toContain(item);
    expect(container.textContent).not.toContain("I have reviewed and agree");
    expect(button("Save").disabled).toBe(true);
    await click(button("Save"));
    expect(update).not.toHaveBeenCalled();
  });

  test("refreshing a confirmed preview requires reviewing its new revision before another save", async () => {
    const nextPreview = { ...preview, revision: "review-2", share_links: 5 };
    const load = vi.spyOn(api, "guestConversion").mockResolvedValue(nextPreview);
    await edit();
    await click(button("Guest"));
    await input("#u-me-password", "example-admin-password");
    await confirmReview();
    expect(button("Save").disabled).toBe(false);
    await click(button("Refresh preview"));
    await settled(() => expect(container.textContent).toContain("Share links: 5"));
    expect(load).toHaveBeenCalledWith(account.id);
    expect(button("Save").disabled).toBe(true);
    expect(checkbox("I have reviewed and agree to these guest account changes").getAttribute("aria-checked")).toBe("false");
    await confirmReview();
    expect(button("Save").disabled).toBe(false);
  });

  test("a stale conversion refusal stays visible and does not close the dialog or announce success", async () => {
    const update = vi.spyOn(api, "updateUser").mockRejectedValue(new ApiError("Guest conversion preview changed. Refresh the preview and confirm again.", 409));
    await edit();
    await click(button("Guest"));
    await input("#u-me-password", "example-admin-password");
    await confirmReview();
    await click(button("Save"));
    await settled(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("Refresh the preview and confirm again."));
    expect(update).toHaveBeenCalledTimes(1);
    expect(close).not.toHaveBeenCalled();
    expect(toast.success).not.toHaveBeenCalled();
    expect(container.querySelector("#u-pw")?.getAttribute("aria-describedby")).toBe("u-error");
  });

  test("a failed preview is an actionable error rather than an empty review, and retry enables review", async () => {
    qc.removeQueries({ queryKey: reviewKey });
    const load = vi.spyOn(api, "guestConversion").mockRejectedValueOnce(new Error("Preview unavailable")).mockResolvedValueOnce(preview);
    await edit();
    await click(button("Guest"));
    await settled(() => expect(container.querySelector('[role="alert"]')?.textContent).toContain("Preview unavailable"));
    expect(button("Save").disabled).toBe(true);
    await click(button("Try again"));
    await settled(() => expect(container.textContent).toContain("Guest conversion preview"));
    expect(load).toHaveBeenCalledTimes(2);
    expect(button("Save").disabled).toBe(true);
    expect(checkbox("I have reviewed and agree to these guest account changes")).toBeDefined();
  });

  test("an administrator cannot change their own account type away from administrator or disable themselves", async () => {
    await edit({ ...account, role: "admin" }, true);
    expect(button("Standard user").disabled).toBe(true);
    expect(button("Guest").disabled).toBe(true);
    await click(button("Guest"));
    expect(button("Administrator").getAttribute("aria-pressed")).toBe("true");
    expect(container.textContent).not.toContain("Disable this account");
    for (const name of ["Edit", "Delete", "Share"]) {
      expect(checkbox(name).getAttribute("aria-checked")).toBe("true");
      expect(checkbox(name).getAttribute("aria-disabled")).toBe("true");
    }
    await click(button("Cancel"));
    expect(close).toHaveBeenCalledTimes(1);
  });

  test("creating a guest excludes My files even when personal spaces are the system default", async () => {
    const create = vi.spyOn(api, "createUser").mockResolvedValue({ ...account, guest: true, personal_space: false });
    await edit(null);
    expect(button("Create").disabled).toBe(true);
    expect(container.querySelector<HTMLInputElement>("#u-quota")?.value).toBe("2");
    await input("#u-name", "guest-amy");
    await input("#u-pw", "example-guest-password");
    await input("#u-display", "Guest Amy");
    await input("#u-quota", "0.5");
    await click(button("Guest"));
    const files = checkbox('Create "My files" (a private space only they can see)');
    expect(files.getAttribute("aria-disabled")).toBe("true");
    expect(files.getAttribute("aria-checked")).toBe("false");
    expect(container.querySelector("select")).toBeNull();
    await click(button("Create"));
    await settled(() => expect(close).toHaveBeenCalledTimes(1));
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({
        username: "guest-amy",
        display_name: "Guest Amy",
        guest: true,
        role: "user",
        personal_space: false,
        personal_location: undefined,
        quota_bytes: 0.5 * 1024 ** 3,
      }),
    );
  });

  test("changing a guest back to standard still requires identity and exposes personal-space management", async () => {
    const guest = { ...account, guest: true, personal_space: false };
    const update = vi.spyOn(api, "updateUser").mockResolvedValue({ ...guest, guest: false });
    await edit(guest);
    expect(container.textContent).not.toContain('Create "My files"…');
    await click(button("Standard user"));
    await click(button('Create "My files"…'));
    expect(personal).toHaveBeenCalledWith("add", guest);
    expect(button("Save").disabled).toBe(true);
    await input("#u-me-password", "example-admin-password");
    await click(button("Save"));
    await settled(() => expect(close).toHaveBeenCalledTimes(1));
    expect(update).toHaveBeenCalledWith(account.id, expect.objectContaining({ guest: false, confirm_guest: false, guest_revision: undefined, my_password: "example-admin-password" }));
  });
});

describe("access end dates", () => {
  const now = new Date(2026, 0, 10, 12, 0).getTime();
  const seconds = (days: number) => Math.floor(now / 1000) + days * 86400;
  const dateText = (days: number) => {
    const date = new Date(now + days * 86400000);
    return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
  };

  test("guest access needs a future end date inside policy, including the exact maximum day", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    const save = vi.fn<(expiry: number | null) => void>();
    await show(<GrantExpiry expiry={null} guest maxDays={7} pending={false} onSave={save} />);
    const field = container.querySelector<HTMLInputElement>('input[aria-label="Access end date"]')!;
    expect(field.required).toBe(true);
    expect(field.max).toBe(dateText(7));
    expect(button("Save end date").disabled).toBe(true);
    for (const days of [-1, 0, 8]) {
      await input('input[aria-label="Access end date"]', dateText(days));
      expect(field.getAttribute("aria-invalid")).toBe("true");
      expect(button("Save end date").disabled).toBe(true);
      await click(button("Save end date"));
    }
    expect(save).not.toHaveBeenCalled();
    await input('input[aria-label="Access end date"]', dateText(7));
    expect(field.getAttribute("aria-invalid")).toBe("false");
    await click(button("Save end date"));
    expect(save).toHaveBeenCalledExactlyOnceWith(seconds(7));
  });

  test("standard access may remove its end date and is not limited by the guest policy", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    const save = vi.fn<(expiry: number | null) => void>();
    await show(<GrantExpiry expiry={seconds(1)} guest={false} maxDays={7} pending={false} onSave={save} />);
    expect(button("Save end date").disabled).toBe(true);
    const field = container.querySelector<HTMLInputElement>("input")!;
    expect(field.required).toBe(false);
    expect(field.hasAttribute("max")).toBe(false);
    await input("input", "");
    await click(button("Save end date"));
    expect(save).toHaveBeenCalledWith(null);
    await input("input", dateText(30));
    await click(button("Save end date"));
    expect(save.mock.calls).toEqual([[null], [seconds(30)]]);
  });

  test("a pending end-date update cannot submit a second change", async () => {
    vi.spyOn(Date, "now").mockReturnValue(now);
    const save = vi.fn<(expiry: number | null) => void>();
    await show(<GrantExpiry expiry={seconds(1)} guest maxDays={7} pending onSave={save} />);
    await input("input", dateText(2));
    expect(button("Save end date").disabled).toBe(true);
    await click(button("Save end date"));
    expect(save).not.toHaveBeenCalled();
  });
});
