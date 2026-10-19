// Exercise the real S3 forms, queries and HTTP client, including failures and credential lifetime.
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { keys } from "@/api/queryKeys";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { toast } from "sonner";
import type { S3Bucket, S3Config, S3Key } from "@/api/s3";
import { S3Settings } from "@/admin/system/S3Settings";
import { S3BucketsPage } from "@/admin/users/S3BucketsPage";
import { BucketCard, S3BucketDialog, S3FolderSection } from "@/components/S3Buckets";

vi.mock("sonner", () => ({ toast: { error: vi.fn<(message: string) => void>(), success: vi.fn<(message: string) => void>() } }));
// The frame's navigation is covered separately; keep the page's refresh action available.
vi.mock("@/admin/SettingsFrame", () => ({
  SettingsFrame: ({ children, onRefresh, searchPlaceholder, onSearch }: { children: ReactNode; onRefresh(): void; searchPlaceholder?: string; onSearch?(q: string): void }) => (
    <main>
      <button onClick={onRefresh}>Refresh</button>
      {/* The frame's search box, when the page searches what it lists */}
      {onSearch && <input placeholder={searchPlaceholder} onChange={(e) => onSearch(e.target.value)} />}
      {children}
    </main>
  ),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const config: S3Config = { enabled: false, hostname: "", region: "us-east-1", managers_publish: false };
const key: S3Key = {
  id: "key-1",
  bucket_id: "bucket-1",
  access_key: "test-access",
  description: "Backup reader",
  writable: false,
  enabled: true,
  expires_at: null,
  created_at: 1,
  last_used_at: null,
};
const bucket: S3Bucket = {
  id: "bucket-1",
  name: "client-backups",
  node_id: "folder-1",
  creator_id: 2,
  enabled: true,
  active: true,
  created_at: 1,
  last_used_at: null,
  creator: "amy",
  folder: "Client",
  keys: [key],
};
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}
let root: Root | null;
let qc: QueryClient;
const fetchMock = vi.fn<typeof fetch>();
const clipboard = vi.fn<(text: string) => Promise<void>>();
function mount(element: ReactNode) {
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  qc = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } } });
  act(() => root!.render(<QueryClientProvider client={qc}>{element}</QueryClientProvider>));
}
const settle = (assertion: () => void) => act(async () => void (await vi.waitFor(assertion)));
const button = (name: string) => {
  const found = [...document.querySelectorAll<HTMLButtonElement>("button")].find((item) => item.textContent?.trim() === name);
  expect(found, `button ${name}`).toBeDefined();
  return found!;
};
const input = (label: string) => {
  const found =
    document.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`) ??
    (([...document.querySelectorAll("label")].find((item) => item.textContent?.trim() === label) as HTMLLabelElement | undefined)?.control as HTMLInputElement | null) ??
    [...document.querySelectorAll("label")].find((item) => item.textContent?.trim() === label)?.querySelector<HTMLInputElement>("input");
  expect(found, `input ${label}`).toBeDefined();
  return found!;
};
function type(label: string, value: string) {
  const field = input(label);
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
/** The current password asked for before credentials are made (`useConfirmIdentity`'s field, labelled by `for`) */
function typePassword(form: "s3-publish" | "s3-key", value: string) {
  const field = document.getElementById(`${form}-password`) as HTMLInputElement | null;
  expect(field, `${form} password`).not.toBeNull();
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field!.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const click = (name: string) => act(() => button(name).click());
const submit = (name: string) => act(() => button(name).form!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
const requests = (method: string) => fetchMock.mock.calls.filter(([, init]) => init?.method === method);
const payload = (call: (typeof fetchMock.mock.calls)[number]) => JSON.parse(call[1]!.body as string) as Record<string, unknown>;
function folderResponses(buckets: S3Bucket[], canPublish = true) {
  fetchMock.mockImplementation(async (url) => (String(url) === "/api/s3/options" ? json({ ...config, hostname: "s3.files.example.com" }) : json({ can_publish: canPublish, buckets })));
}
beforeEach(() => {
  fetchMock.mockReset();
  clipboard.mockReset().mockResolvedValue(undefined);
  vi.clearAllMocks();
  vi.stubGlobal("fetch", fetchMock);
  vi.spyOn(navigator.clipboard, "writeText").mockImplementation(clipboard);
});
afterEach(() => {
  act(() => root?.unmount());
  root = null;
  qc?.clear();
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("S3 settings", () => {
  test("loading, a failed read and retry lead to safe disabled defaults, and saving sends the edited policy", async () => {
    const load = deferred<Response>();
    const save = deferred<Response>();
    fetchMock
      .mockImplementationOnce(() => load.promise)
      .mockResolvedValueOnce(json(config))
      .mockImplementationOnce(() => save.promise)
      .mockResolvedValueOnce(json({ ...config, enabled: true, hostname: "s3.files.example.com", region: "eu-west-1", managers_publish: true }));
    mount(<S3Settings />);
    expect(document.querySelector('[role="status"]')?.textContent).toContain("Loading");
    await act(async () => load.resolve(json({ error: "Storage settings unavailable" }, 503)));
    await settle(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("Storage settings unavailable"));
    click("Try again");
    await settle(() => expect(input("Enable S3 access").checked).toBe(false));
    expect(input("Space managers can publish buckets").checked).toBe(false);
    expect(input("S3 hostname").required).toBe(false);
    act(() => input("Enable S3 access").click());
    act(() => input("Space managers can publish buckets").click());
    expect(input("S3 hostname").required).toBe(true);
    type("S3 hostname", "s3.files.example.com");
    type("Region", "eu-west-1");
    submit("Save");
    await settle(() => expect(button("Saving…").disabled).toBe(true));
    expect(payload(requests("PUT")[0])).toEqual({ enabled: true, hostname: "s3.files.example.com", region: "eu-west-1", managers_publish: true });
    await act(async () => save.resolve(json({ error: "Invalid S3 hostname" }, 400)));
    await settle(() => expect(toast.error).toHaveBeenCalledWith("Invalid S3 hostname"));
    await settle(() => expect(button("Save").disabled).toBe(false));
    expect(input("S3 hostname").value).toBe("s3.files.example.com");
    submit("Save");
    await settle(() => expect(toast.success).toHaveBeenCalledWith("S3 settings updated"));
    expect(qc.getQueryData(["s3-config"])).toEqual({ ...config, enabled: true, hostname: "s3.files.example.com", region: "eu-west-1", managers_publish: true });
  });
});

describe("folder buckets", () => {
  test("a failed list can be retried, and a folder without publication permission has no publish form", async () => {
    const load = deferred<Response>();
    const close = vi.fn<() => void>();
    fetchMock.mockImplementation(async (url) => (String(url) === "/api/s3/options" ? json({ ...config, hostname: "s3.files.example.com" }) : load.promise));
    mount(<S3BucketDialog nodeId="folder-1" onClose={close} />);
    await settle(() => expect(document.querySelector('[role="status"]')?.textContent).toContain("Loading"));
    await act(async () => load.resolve(json({ error: "Could not load buckets" }, 503)));
    await settle(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("Could not load buckets"));
    folderResponses([], false);
    click("Try again");
    await settle(() => expect(document.body.textContent).toContain("This folder can't be published as an S3 bucket"));
    expect(document.body.textContent).toContain("s3.files.example.com");
    expect(document.body.textContent).toContain("Use path-style URLs");
    expect(document.querySelector('input[aria-label="Bucket name"]')).toBeNull();
    expect(String(fetchMock.mock.calls[0][0])).toBe("/api/s3/buckets?node_id=folder-1");
    click("Close");
    expect(close).toHaveBeenCalledTimes(1);
  });

  test("publication trims the name, blocks duplicate submission while pending, and refreshes the folder after retry", async () => {
    const publish = deferred<Response>();
    let published = false;
    let attempts = 0;
    fetchMock.mockImplementation(async (url, init) => {
      if (init?.method === "POST") {
        attempts++;
        if (attempts === 1) return publish.promise;
        published = true;
        return json(bucket);
      }
      return String(url) === "/api/s3/options" ? json({ ...config, hostname: "s3.files.example.com" }) : json({ can_publish: true, buckets: published ? [bucket] : [] });
    });
    mount(<S3BucketDialog nodeId="folder-1" onClose={() => {}} />);
    await settle(() => expect(input("Bucket name").value).toBe(""));
    type("Bucket name", "  client-backups  ");
    // Publishing asks who is acting, as making other credentials does
    expect(button("Publish").disabled).toBe(true);
    typePassword("s3-publish", "current-password");
    submit("Publish");
    await settle(() => expect(button("Publish").disabled).toBe(true));
    expect(payload(requests("POST")[0])).toEqual({ node_id: "folder-1", name: "client-backups", password: "current-password" });
    await act(async () => publish.resolve(json({ error: "Bucket name is already in use" }, 409)));
    await settle(() => expect(toast.error).toHaveBeenCalledWith("Bucket name is already in use"));
    await settle(() => expect(button("Publish").disabled).toBe(false));
    expect(input("Bucket name").value).toBe("  client-backups  ");
    expect((document.getElementById("s3-publish-password") as HTMLInputElement).value).toBe("current-password");
    submit("Publish");
    await settle(() => expect(document.querySelector('section[aria-label="client-backups"]')).not.toBeNull());
    expect(input("Bucket name").value).toBe("");
    expect(requests("POST")).toHaveLength(2);
  });

  test("the properties section hides unavailable publication and opens management for an existing bucket", async () => {
    folderResponses([], false);
    mount(<S3FolderSection nodeId="folder-1" />);
    await settle(() => expect(qc.getQueryState(keys.buckets("folder-1"))?.status).toBe("success"));
    expect(document.body.textContent).toBe("");
    folderResponses([{ ...bucket, enabled: false, active: false, keys: [] }], false);
    await act(async () => void (await qc.invalidateQueries({ queryKey: keys.bucketLists() })));
    await settle(() => expect(document.body.textContent).toContain("client-backups · 0 Access Keys"));
    click("Manage buckets and keys");
    await settle(() => expect(document.querySelector('[role="dialog"]')?.textContent).toContain("No Access Keys yet"));
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain("Stopped");
    click("Close");
    await settle(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  });
});

describe("Access Keys", () => {
  test("a new key defaults to read-only with no expiry and its secret disappears when creation is closed", async () => {
    const create = deferred<Response>();
    fetchMock.mockImplementation(async (_url, init) => (init?.method === "POST" ? create.promise : json({ enabled: false, has_password: true })));
    mount(<BucketCard bucket={bucket} />);
    click("New S3 key");
    await settle(() => expect(input("Read and change files").checked).toBe(false));
    expect(button("Create key").disabled).toBe(true);
    type("Key description", "   ");
    expect(button("Create key").disabled).toBe(true);
    type("Key description", "  Backup reader  ");
    expect(button("Create key").disabled).toBe(true);
    typePassword("s3-key", "current-password");
    submit("Create key");
    await settle(() => expect(button("Creating…").disabled).toBe(true));
    expect(String(requests("POST")[0][0])).toBe("/api/s3/buckets/bucket-1/keys");
    expect(payload(requests("POST")[0])).toEqual({ description: "Backup reader", writable: false, expires_at: null, password: "current-password" });
    await act(async () => create.resolve(json({ ...key, secret_key: "test-secret-shown-once" })));
    await settle(() => expect(input("Access Key").readOnly).toBe(true));
    expect(input("Secret Key").readOnly).toBe(true);
    expect(input("Secret Key").value).toBe("test-secret-shown-once");
    // As dots until Show is pressed
    expect(input("Secret Key").type).toBe("password");
    clipboard.mockRejectedValueOnce(new DOMException("Permission denied", "NotAllowedError"));
    click("Copy credentials");
    await settle(() => expect(toast.error).toHaveBeenCalledWith("Couldn't copy"));
    expect(input("Secret Key").value).toBe("test-secret-shown-once");
    click("Copy credentials");
    await settle(() => expect(clipboard).toHaveBeenLastCalledWith("AWS_ACCESS_KEY_ID=test-access\nAWS_SECRET_ACCESS_KEY=test-secret-shown-once"));
    expect(toast.success).toHaveBeenCalledWith("Copied");
    click("Done");
    await settle(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
    click("New S3 key");
    await settle(() => expect(document.body.textContent).not.toContain("Save the Secret Key now"));
    expect(document.querySelector('input[value="test-secret-shown-once"]')).toBeNull();
    expect(input("Key description").value).toBe("");
    click("Close");
    await settle(() => expect(document.querySelector('[role="dialog"]')).toBeNull());
  });

  test("write access and the chosen end date are explicit, and a refused creation preserves the form for retry", async () => {
    const answers = [json({ error: "S3 key limit reached" }, 400), json({ ...key, writable: true, secret_key: "test-retry-secret" })];
    fetchMock.mockImplementation(async (_url, init) => (init?.method === "POST" ? answers.shift()! : json({ enabled: false, has_password: true })));
    mount(<BucketCard bucket={bucket} />);
    click("New S3 key");
    await settle(() => expect(input("Key description").value).toBe(""));
    type("Key description", " Writer ");
    act(() => input("Read and change files").click());
    type("End date (optional)", "2030-05-01T12:30");
    typePassword("s3-key", "current-password");
    submit("Create key");
    await settle(() => expect(toast.error).toHaveBeenCalledWith("S3 key limit reached"));
    await settle(() => expect(button("Create key").disabled).toBe(false));
    expect(input("Key description").value).toBe(" Writer ");
    expect(input("Read and change files").checked).toBe(true);
    submit("Create key");
    await settle(() => expect(input("Secret Key").value).toBe("test-retry-secret"));
    const sent = { description: "Writer", writable: true, expires_at: Math.floor(new Date("2030-05-01T12:30").getTime() / 1000), password: "current-password" };
    expect(requests("POST").map(payload)).toEqual([sent, sent]);
    expect(input("Secret Key").value).toBe("test-retry-secret");
  });

  test("revoking a key and stopping a bucket send only their own enabled flags and refresh current status", async () => {
    const revoke = deferred<Response>();
    let current = { ...bucket, last_used_at: 1700000000, keys: [{ ...key, writable: true, expires_at: 1900000000, last_used_at: 1700000000 }] };
    let failed = false;
    fetchMock.mockImplementation(async (url, init) => {
      if (init?.method === "PATCH") {
        if (String(url).endsWith("/keys/key-1") && !failed) {
          failed = true;
          return revoke.promise;
        }
        const enabled = JSON.parse(init.body as string).enabled as boolean;
        current = String(url).endsWith("/keys/key-1") ? { ...current, keys: [{ ...current.keys[0], enabled }] } : { ...current, enabled, active: enabled };
        return json({});
      }
      return String(url) === "/api/s3/options" ? json(config) : json({ can_publish: false, buckets: [current] });
    });
    mount(<S3BucketDialog nodeId="folder-1" onClose={() => {}} />);
    await settle(() => expect(document.body.textContent).toContain("Read and change files"));
    expect(document.body.textContent).not.toContain("No end date");
    click("Revoke key");
    await settle(() => expect(button("Revoke key").disabled).toBe(true));
    await act(async () => revoke.resolve(json({ error: "The key couldn't be updated" }, 503)));
    await settle(() => expect(toast.error).toHaveBeenCalledWith("The key couldn't be updated"));
    await settle(() => expect(button("Revoke key").disabled).toBe(false));
    click("Revoke key");
    await settle(() => expect(button("Enable key").disabled).toBe(false));
    click("Enable key");
    await settle(() => expect(button("Revoke key")).toBeDefined());
    click("Stop bucket");
    await settle(() => expect(button("Enable bucket")).toBeDefined());
    click("Enable bucket");
    await settle(() => expect(button("Stop bucket")).toBeDefined());
    expect(requests("PATCH").map(([url, init]) => [String(url), JSON.parse(init!.body as string)])).toEqual([
      ["/api/s3/buckets/bucket-1/keys/key-1", { enabled: false }],
      ["/api/s3/buckets/bucket-1/keys/key-1", { enabled: false }],
      ["/api/s3/buckets/bucket-1/keys/key-1", { enabled: true }],
      ["/api/s3/buckets/bucket-1", { enabled: false }],
      ["/api/s3/buckets/bucket-1", { enabled: true }],
    ]);
  });

  test("deletion needs confirmation; cancellation sends nothing and a failed deletion stays open for retry", async () => {
    const remove = deferred<Response>();
    let current: S3Bucket[] = [bucket];
    let attempts = 0;
    fetchMock.mockImplementation(async (url, init) => {
      if (init?.method === "DELETE") {
        attempts++;
        if (attempts === 1) return remove.promise;
        current = String(url).endsWith("/keys/key-1") ? [{ ...bucket, keys: [] }] : [];
        return json({});
      }
      return String(url) === "/api/s3/options" ? json(config) : json({ can_publish: false, buckets: current });
    });
    mount(<S3BucketDialog nodeId="folder-1" onClose={() => {}} />);
    await settle(() => expect(button("Delete key")).toBeDefined());
    click("Delete key");
    await settle(() => expect(document.body.textContent).toContain("Delete key?"));
    expect(document.body.textContent).toContain("Files stay in their folder");
    expect(requests("DELETE")).toHaveLength(0);
    click("Cancel");
    await settle(() => expect(document.body.textContent).not.toContain("Delete key?"));
    expect(requests("DELETE")).toHaveLength(0);
    click("Delete key");
    await settle(() => expect(document.body.textContent).toContain("Delete key?"));
    click("Delete");
    await settle(() => expect(button("Delete").disabled).toBe(true));
    await act(async () => remove.resolve(json({ error: "The key is locked" }, 409)));
    await settle(() => expect(document.querySelector('[role="alert"]')?.textContent).toBe("The key is locked"));
    click("Delete");
    await settle(() => expect(document.body.textContent).toContain("No Access Keys yet"));
    expect(document.body.textContent).not.toContain("Delete key?");
    click("Delete bucket");
    await settle(() => expect(document.body.textContent).toContain("Delete bucket?"));
    click("Cancel");
    await settle(() => expect(document.body.textContent).not.toContain("Delete bucket?"));
    expect(requests("DELETE")).toHaveLength(2);
    click("Delete bucket");
    await settle(() => expect(document.body.textContent).toContain("Delete bucket?"));
    click("Delete");
    await settle(() => expect(document.querySelector('section[aria-label="client-backups"]')).toBeNull());
    expect(requests("DELETE").map(([url]) => String(url))).toEqual(["/api/s3/buckets/bucket-1/keys/key-1", "/api/s3/buckets/bucket-1/keys/key-1", "/api/s3/buckets/bucket-1"]);
  });
});

test("the administrator's bucket list retries failed loads, searches on the server from the page's search box and says when nothing matches", async () => {
  const load = deferred<Response>();
  let current = [bucket, { ...bucket, id: "bucket-2", name: "photos", creator: "ben", folder: "Travel", keys: [] }];
  // The server searches the bucket's name, its creator's and its folder's
  const matching = (url: string) => {
    const search = new URL(url, "http://files.example.com").searchParams.get("search")?.toLowerCase();
    return search ? current.filter((b) => `${b.name} ${b.creator} ${b.folder}`.toLowerCase().includes(search)) : current;
  };
  fetchMock.mockImplementationOnce(() => load.promise).mockImplementation(async (url) => json({ can_publish: true, buckets: matching(String(url)) }));
  mount(<S3BucketsPage />);
  expect(document.querySelector('[role="status"]')?.textContent).toContain("Loading");
  await act(async () => load.resolve(json({ error: "Bucket list unavailable" }, 503)));
  await settle(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("Bucket list unavailable"));
  click("Try again");
  await settle(() => expect(document.querySelector('section[aria-label="client-backups"]')).not.toBeNull());
  // A page at a time
  expect(String(fetchMock.mock.calls[0][0])).toBe("/api/s3/buckets?limit=100");
  // One search box, the page's own
  const boxes = document.querySelectorAll<HTMLInputElement>('input[placeholder="Search buckets"]');
  expect(boxes).toHaveLength(1);
  const search = async (text: string) => {
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      set.call(boxes[0], text);
      boxes[0].dispatchEvent(new Event("input", { bubbles: true }));
    });
    // The page waits for a pause in typing before it asks the server
    await act(async () => new Promise((r) => setTimeout(r, 300)));
  };
  await search("travel");
  // The buckets shown stay while the new answer loads; then only what matches
  await settle(() => expect(document.querySelector('section[aria-label="client-backups"]')).toBeNull());
  expect(document.querySelector('section[aria-label="photos"]')).not.toBeNull();
  expect(fetchMock.mock.calls.some(([url]) => String(url).includes("search=travel"))).toBe(true);
  await search("nothing-like-it");
  await settle(() => expect(document.body.textContent).toContain('No buckets match "nothing-like-it"'));
  await search("");
  await settle(() => expect(document.querySelector('section[aria-label="photos"]')).not.toBeNull());
  current = [];
  click("Refresh");
  await settle(() => expect(document.body.textContent).toContain("No S3 buckets yet"));
});

test("the administrator's bucket list shows more when a page is full, asking for the buckets after the last one", async () => {
  const many = Array.from({ length: 101 }, (_, i) => ({ ...bucket, id: `bucket-${String(i).padStart(3, "0")}`, name: `bucket-${i}`, created_at: 1000 - i, keys: [] }));
  // The server's pages: newest first, after the bucket the page before ended with
  fetchMock.mockImplementation(async (url) => {
    const params = new URL(String(url), "http://files.example.com").searchParams;
    const after = params.get("after_id");
    const start = after ? many.findIndex((b) => b.id === after) + 1 : 0;
    return json({ can_publish: true, buckets: many.slice(start, start + Number(params.get("limit"))) });
  });
  mount(<S3BucketsPage />);
  await settle(() => expect(document.querySelectorAll("section[aria-label]")).toHaveLength(100));
  click("Show more");
  await settle(() => expect(document.querySelectorAll("section[aria-label]")).toHaveLength(101));
  const second = new URL(String(fetchMock.mock.calls.at(-1)![0]), "http://files.example.com").searchParams;
  expect([second.get("after_created"), second.get("after_id"), second.get("limit")]).toEqual(["901", "bucket-099", "100"]);
  // The last page wasn't full: nothing more to ask for
  expect([...document.querySelectorAll("button")].some((b) => b.textContent === "Show more")).toBe(false);
});
