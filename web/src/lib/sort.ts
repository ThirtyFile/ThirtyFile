//! The order folders are listed in, kept for the next visit

import { SORT_KEYS, type SortKey, type SortOrder } from "@/api";
import { usePersisted } from "@/lib/session";

export function useSort() {
  const [sort, setSort] = usePersisted<{ key: SortKey; order: SortOrder }>(
    "tf-sort",
    { key: "name", order: "asc" },
    (s) => SORT_KEYS.includes(s.key) && (s.order === "asc" || s.order === "desc"),
  );
  const toggle = (key: SortKey) => setSort({ key, order: sort.key === key && sort.order === "asc" ? "desc" : "asc" });
  return [sort, toggle, setSort] as const;
}
