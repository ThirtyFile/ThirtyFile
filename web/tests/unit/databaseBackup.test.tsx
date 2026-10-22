// Database backups: a passphrase is typed twice and kept when left blank, and the Backups page says what happened last
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, test, vi } from "vite-plus/test";
import type { DatabaseBackup, StorageLocation } from "@/api";
import { DatabaseBackupDialog, databaseBackupSummary } from "@/admin/storage/DatabaseBackupDialog";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const requests = vi.hoisted(() => ({
  databaseBackup: vi.fn<typeof import("@/api").api.databaseBackup>(),
  saveDatabaseBackup: vi.fn<typeof import("@/api").api.saveDatabaseBackup>(),
  runDatabaseBackup: vi.fn<typeof import("@/api").api.runDatabaseBackup>(),
  databaseCopies: vi.fn<typeof import("@/api").api.databaseCopies>(),
  backupNextRuns: vi.fn<typeof import("@/api").api.backupNextRuns>(),
  storageLocations: vi.fn<typeof import("@/api").api.storageLocations>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});

const off: DatabaseBackup = {
  enabled: false,
  location: "",
  schedule: { daily: "02:30" },
  tz: "UTC",
  keep_last: 7,
  keep_daily: 7,
  keep_weekly: 4,
  keep_monthly: 12,
  alert_hours: 48,
  has_passphrase: false,
  running: false,
  status: { next_run_at: null, last_attempt_at: null, last_success_at: null, last_file: null, last_size: null, last_duration_ms: null, error: null },
};
const nas = { id: "nas", name: "NAS", kind: "local", connected: true } as StorageLocation;

let root: Root;
beforeEach(() => {
  Object.values(requests).forEach((r) => r.mockReset());
  requests.storageLocations.mockResolvedValue([nas]);
  requests.backupNextRuns.mockResolvedValue([]);
  requests.databaseCopies.mockResolvedValue([]);
  requests.saveDatabaseBackup.mockImplementation(async (s) => ({ ...off, ...s, has_passphrase: true }));
});
afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});

function mount(current: DatabaseBackup) {
  requests.databaseBackup.mockResolvedValue(current);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  act(() =>
    root.render(
      <QueryClientProvider client={qc}>
        <DatabaseBackupDialog onClose={() => {}} />
      </QueryClientProvider>,
    ),
  );
}
const settle = (check: () => void) => act(async () => vi.waitFor(check));
const button = (name: string) => [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === name)!;
async function type(selector: string, value: string) {
  const el = document.querySelector<HTMLInputElement | HTMLSelectElement>(selector)!;
  await act(async () => {
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
}

test("a first passphrase is typed twice, as long as asked, before the copies can be turned on", async () => {
  mount(off);
  await settle(() => expect(document.querySelector("#dbb-location option[value='nas']")).not.toBeNull());
  await type("#dbb-location", "nas");
  // On, without a passphrase: not yet
  expect(button("Save").disabled).toBe(true);
  await type("#dbb-passphrase", "short");
  expect(document.body.textContent).toContain("The passphrase needs at least 12 characters");
  await type("#dbb-passphrase", "a long enough passphrase");
  expect(document.querySelector<HTMLInputElement>("#dbb-passphrase")!.type).toBe("password");
  await type("#dbb-passphrase-again", "a different passphrase!");
  expect(document.body.textContent).toContain("The two passphrases aren't the same");
  expect(button("Save").disabled).toBe(true);
  await type("#dbb-passphrase-again", "a long enough passphrase");
  expect(button("Save").disabled).toBe(false);
  await act(async () => button("Save").click());
  await settle(() => expect(requests.saveDatabaseBackup).toHaveBeenCalledTimes(1));
  expect(requests.saveDatabaseBackup.mock.calls[0][0]).toMatchObject({ enabled: true, location: "nas", keep_last: 7, passphrase: "a long enough passphrase" });
});

test("a passphrase left blank keeps the one saved, and a copy can be asked for now", async () => {
  mount({ ...off, enabled: true, location: "nas", has_passphrase: true });
  await settle(() => expect(button("Save").disabled).toBe(false));
  await act(async () => button("Save").click());
  await settle(() => expect(requests.saveDatabaseBackup).toHaveBeenCalledTimes(1));
  expect(requests.saveDatabaseBackup.mock.calls[0][0].passphrase).toBeUndefined();
  requests.runDatabaseBackup.mockResolvedValue({ ...off, enabled: true, location: "nas", has_passphrase: true, running: true });
  await act(async () => button("Back up now").click());
  await settle(() => expect(requests.runDatabaseBackup).toHaveBeenCalledTimes(1));
});

test("the page says what the database backups did last", () => {
  expect(databaseBackupSummary(off)).toBe("Database backups are off");
  const on = { ...off, enabled: true, location: "nas", has_passphrase: true };
  expect(databaseBackupSummary(on)).toBe("Database: no copy yet");
  expect(databaseBackupSummary({ ...on, running: true })).toBe("Database: a copy is being made");
  expect(databaseBackupSummary({ ...on, status: { ...on.status, error: "Disk full", last_success_at: 1 } })).toBe("Database: the last copy failed");
  expect(databaseBackupSummary({ ...on, status: { ...on.status, last_success_at: 1_792_375_800 } })).toMatch(/^Database: copied /);
});
