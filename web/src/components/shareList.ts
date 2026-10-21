//! The share links table: where a link's item is, and how the rows are sorted

import type { ShareInfo } from "@/api";
import { driveName } from "@/api/names";
import { t } from "@/lib/i18n";

export type ShareSortKey = "name" | "location" | "owner" | "created" | "expires" | "downloads" | "views" | "accessed";
export const SHARE_SORT_KEYS: ShareSortKey[] = ["name", "location", "owner", "created", "expires", "downloads", "views", "accessed"];
export interface ShareSort {
  key: ShareSortKey;
  order: "asc" | "desc";
}

/** The space a link's item is in, as the person reads it: their own "My files", other people's by name */
function spaceLabel(s: ShareInfo, me: { username: string }) {
  if (s.drive_kind === "personal" && s.drive_owner !== me.username) return t("My files of {name}", { name: s.drive_owner });
  return driveName({ kind: s.drive_kind, name: s.drive_name });
}

/** Where the item of a link is: its space and the folders above it, e.g. "All files/ABC". A path that starts below the top of the space (a restricted
 * folder above it is hidden) starts with an ellipsis. Links in other people's personal spaces that an administrator may only delete say nothing */
export function shareLocation(s: ShareInfo, me: { username: string }): string {
  if (s.private) return "—";
  return [s.location_partial ? "…" : spaceLabel(s, me), ...s.location_path].join("/");
}

/** The same with the item itself at the end: the full path of what the link opens */
export function sharePathOfItem(s: ShareInfo, me: { username: string }): string {
  return s.private ? "—" : `${shareLocation(s, me)}/${s.node_name}`;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** The rows in the order asked for. Equal values keep the newest link first. A link that never expires sorts as the furthest date, one never
 * accessed as the oldest */
export function sortShares(rows: ShareInfo[], sort: ShareSort, me: { username: string }): ShareInfo[] {
  const dir = sort.order === "asc" ? 1 : -1;
  const text = (s: ShareInfo) => {
    switch (sort.key) {
      case "name":
        return s.node_name;
      case "location":
        return shareLocation(s, me);
      case "owner":
        return s.owner_name;
      default:
        return null;
    }
  };
  const number = (s: ShareInfo) => {
    switch (sort.key) {
      case "created":
        return s.created_at;
      case "expires":
        return s.expires_at ?? Number.POSITIVE_INFINITY;
      case "downloads":
        return s.downloads;
      case "views":
        return s.views;
      case "accessed":
        return s.last_access ?? 0;
      default:
        return 0;
    }
  };
  return [...rows].sort((a, b) => {
    const [ta, tb] = [text(a), text(b)];
    const by = ta !== null && tb !== null ? collator.compare(ta, tb) : number(a) === number(b) ? 0 : number(a) < number(b) ? -1 : 1;
    // A location that is the same is told by the name; then the newest link first, and the address, so the order never jumps
    const then = sort.key === "location" ? collator.compare(a.node_name, b.node_name) : 0;
    return by * dir || then * dir || b.created_at - a.created_at || (a.id < b.id ? -1 : 1);
  });
}
