// The share links table: where a link's item is, and the order of the rows
import { expect, test } from "vite-plus/test";
import type { ShareInfo } from "@/api";
import { shareLocation, sharePathOfItem, sortShares, type ShareSort } from "@/components/shareList";

const me = { username: "amy" };

function share(over: Partial<ShareInfo>): ShareInfo {
  return {
    id: "a",
    node_id: "n",
    has_password: false,
    expires_at: null,
    max_downloads: null,
    downloads: 0,
    created_at: 1000,
    node_name: "XXX",
    node_kind: "folder",
    views: 0,
    last_access: null,
    owner_id: 1,
    owner_name: "amy",
    drive_id: "d",
    drive_name: "Marketing",
    drive_kind: "team",
    drive_owner: "",
    location_path: [],
    location_partial: false,
    location_id: "p",
    allow_upload: false,
    drop_only: false,
    allow_download: true,
    ...over,
  };
}

const ids = (rows: ShareInfo[]) => rows.map((s) => s.id).join(",");
const by = (key: ShareSort["key"], order: ShareSort["order"] = "asc"): ShareSort => ({ key, order });

test("two folders with the same name are told apart by their path", () => {
  const abc = share({ id: "1", location_path: ["ABC"] });
  const efg = share({ id: "2", location_path: ["EFG"] });
  expect([shareLocation(abc, me), shareLocation(efg, me)]).toEqual(["Marketing/ABC", "Marketing/EFG"]);
  expect([sharePathOfItem(abc, me), sharePathOfItem(efg, me)]).toEqual(["Marketing/ABC/XXX", "Marketing/EFG/XXX"]);
  // At the top of the space, only the space
  expect(shareLocation(share({}), me)).toBe("Marketing");
});

test("a person's own files, other people's files, the company space, a path cut by a restricted folder and a private link", () => {
  expect(shareLocation(share({ drive_kind: "personal", drive_name: "My files", drive_owner: "amy", location_path: ["Inbox"] }), me)).toBe("My files/Inbox");
  expect(shareLocation(share({ drive_kind: "personal", drive_name: "My files", drive_owner: "ben" }), me)).toBe("My files of ben");
  expect(shareLocation(share({ drive_kind: "company", drive_name: "All files", location_path: ["Reports", "2026"] }), me)).toBe("All files/Reports/2026");
  expect(shareLocation(share({ location_partial: true, location_path: ["2026"] }), me)).toBe("…/2026");
  expect(shareLocation(share({ location_partial: true, location_path: [] }), me)).toBe("…");
  expect(shareLocation(share({ private: true, node_name: "", location_path: [] }), me)).toBe("—");
  expect(sharePathOfItem(share({ private: true }), me)).toBe("—");
});

test("sorting by name or location, in both directions, with numbers read as numbers", () => {
  const rows = [
    share({ id: "1", node_name: "report 10.txt", location_path: ["B"] }),
    share({ id: "2", node_name: "Report 2.txt", location_path: ["A"] }),
    share({ id: "3", node_name: "apple.txt", location_path: ["B"] }),
  ];
  expect(ids(sortShares(rows, by("name"), me))).toBe("3,2,1");
  expect(ids(sortShares(rows, by("name", "desc"), me))).toBe("1,2,3");
  // By location first, and by name inside the same location
  expect(ids(sortShares(rows, by("location"), me))).toBe("2,3,1");
  expect(ids(sortShares(rows, by("location", "desc"), me))).toBe("1,3,2");
});

test("same-named items end up together when sorted by name or location", () => {
  const rows = [
    share({ id: "1", location_path: ["EFG"], created_at: 1 }),
    share({ id: "2", node_name: "other", location_path: ["ABC"], created_at: 2 }),
    share({ id: "3", location_path: ["ABC"], created_at: 3 }),
  ];
  expect(ids(sortShares(rows, by("location"), me))).toBe("2,3,1");
  // Equal names: the newest link first
  expect(
    ids(
      sortShares(
        rows.filter((s) => s.node_name === "XXX"),
        by("name"),
        me,
      ),
    ),
  ).toBe("3,1");
});

test("a link that never expires sorts as the furthest date and one never accessed as the oldest", () => {
  const rows = [
    share({ id: "never", expires_at: null, last_access: null }),
    share({ id: "soon", expires_at: 2000, last_access: 5000 }),
    share({ id: "later", expires_at: 9000, last_access: 3000 }),
  ];
  expect(ids(sortShares(rows, by("expires"), me))).toBe("soon,later,never");
  expect(ids(sortShares(rows, by("expires", "desc"), me))).toBe("never,later,soon");
  expect(ids(sortShares(rows, by("accessed", "desc"), me))).toBe("soon,later,never");
  expect(ids(sortShares(rows, by("accessed"), me))).toBe("never,later,soon");
});

test("sorting by numbers and by who created the link, and the rows given are left as they were", () => {
  const rows = [
    share({ id: "1", downloads: 5, views: 1, created_at: 30, owner_name: "ben" }),
    share({ id: "2", downloads: 9, views: 1, created_at: 10, owner_name: "amy" }),
    share({ id: "3", downloads: 1, views: 7, created_at: 20, owner_name: "Carl" }),
  ];
  const before = ids(rows);
  expect(ids(sortShares(rows, by("downloads", "desc"), me))).toBe("2,1,3");
  expect(ids(sortShares(rows, by("views", "desc"), me))).toBe("3,1,2");
  expect(ids(sortShares(rows, by("created"), me))).toBe("2,3,1");
  expect(ids(sortShares(rows, by("created", "desc"), me))).toBe("1,3,2");
  expect(ids(sortShares(rows, by("owner"), me))).toBe("2,1,3");
  expect(ids(rows)).toBe(before);
});
