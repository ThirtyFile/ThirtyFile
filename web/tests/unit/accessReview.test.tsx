// Access review: what each view shows, personal spaces never named, guests filtered, and the review marked
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { AccessReview } from "@/api";
import { AccessReviewPage, placeLabel } from "@/admin/users/AccessReviewPage";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const requests = vi.hoisted(() => ({
  accessReview: vi.fn<typeof import("@/api").api.accessReview>(),
  setReviewReminder: vi.fn<typeof import("@/api").api.setReviewReminder>(),
  markReviewed: vi.fn<typeof import("@/api").api.markReviewed>(),
}));
vi.mock("@/api", async (original) => {
  const actual = await original<typeof import("@/api")>();
  return { ...actual, api: { ...actual.api, ...requests } };
});
// The Control panel frame needs a router and a signed-in person; the page's own content is what is tested
vi.mock("@/admin/SettingsFrame", () => ({ SettingsFrame: ({ children }: { children: React.ReactNode }) => <div>{children}</div> }));

const team = { space_id: "d1", space: "Projects", space_kind: "team" as const };
const review: AccessReview = {
  generated_at: 1_792_375_800,
  spaces: [
    {
      id: "d1",
      name: "Projects",
      kind: "team",
      members: [{ principal: { kind: "group", id: 3, name: "Design", guest: false }, role: "editor", expires_at: null, granted_at: 1 }],
      folders: [
        {
          place: { ...team, node_id: "n1", path: ["Client"] },
          restricted: true,
          grants: [{ principal: { kind: "user", id: 9, name: "outside", guest: true }, role: "viewer", expires_at: 1_792_400_000, granted_at: 1 }],
        },
      ],
    },
  ],
  personal: [{ owner: "ben", items: 1, grants: 1, links: 1 }],
  expiring: [],
  people: [
    {
      id: 2,
      username: "cat",
      display_name: "",
      guest: false,
      admin: false,
      disabled: false,
      groups: ["Design"],
      reach: [{ place: { ...team, node_id: "r", path: [] }, role: "editor", via: "group", group: "Design", expires_at: null }],
      app_passwords: [],
      ssh_keys: [],
    },
    { id: 9, username: "outside", display_name: "", guest: true, admin: false, disabled: false, groups: [], reach: [], app_passwords: [], ssh_keys: [] },
  ],
  links: [
    {
      place: { space_id: "p1", space: "ben", space_kind: "personal", node_id: null, path: null },
      creator: "ben",
      created_at: 1,
      expires_at: null,
      password: false,
      allow_upload: false,
      allow_download: true,
      downloads: 0,
      max_downloads: null,
      views: 0,
      last_access: null,
    },
  ],
  buckets: [],
  receive_folders: [],
  review: { every_days: 90, last_reviewed_at: null, last_reviewed_by: null, due_at: 1_800_000_000 },
};

let root: Root;
beforeEach(() => {
  Object.values(requests).forEach((r) => r.mockReset());
  requests.accessReview.mockResolvedValue(review);
  requests.markReviewed.mockResolvedValue({ ...review.review, last_reviewed_at: 1_792_375_900, last_reviewed_by: "admin" });
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  act(() =>
    root.render(
      <QueryClientProvider client={qc}>
        <AccessReviewPage />
      </QueryClientProvider>,
    ),
  );
});
afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
});
const settle = (check: () => void) => act(async () => vi.waitFor(check));

test("a place is its space and folders; in a personal space only whose it is", () => {
  expect(placeLabel({ ...team, node_id: "n1", path: ["Client", "2026"] })).toBe("Projects › Client › 2026");
  expect(placeLabel({ ...team, node_id: "r", path: [] })).toBe("Projects");
  expect(placeLabel({ space_id: "p", space: "ben", space_kind: "personal", node_id: null, path: null })).toBe("My files of ben");
});

test("spaces, people and outside access are listed; guests can be shown alone; a review is marked", async () => {
  await settle(() => expect(document.body.textContent).toContain("Spaces and folders"));
  const text = () => document.body.textContent ?? "";
  expect(text()).toContain("Design");
  expect(text()).toContain("Projects › Client");
  expect(text()).toContain("Restricted folder");
  expect(text()).toContain("Through Design");
  expect(text()).toContain("ben — shared items: 1, grants: 1, links: 1");
  expect(text()).toContain("My files of ben");
  expect(text()).toContain("Not reviewed yet");
  // Guests only
  const guests = [...document.querySelectorAll("label")].find((l) => l.textContent === "Guests only")!.querySelector("input")!;
  await act(async () => guests.click());
  const names = [...document.querySelectorAll("details summary")].map((s) => s.querySelector("span")?.textContent);
  expect(names).toEqual(["outside"]);
  const mark = [...document.querySelectorAll("button")].find((b) => b.textContent?.includes("Mark as reviewed"))!;
  await act(async () => mark.click());
  await settle(() => expect(requests.markReviewed).toHaveBeenCalledTimes(1));
});
