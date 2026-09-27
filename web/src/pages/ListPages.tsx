import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { ClockIcon, SearchIcon, SearchXIcon, StarIcon } from "lucide-react";
import { api, type Located, type SortKey, type SortOrder } from "@/api";
import { Explorer } from "@/components/Explorer";
import { useSort } from "@/pages/FilesPage";
import { t } from "@/lib/i18n";
import { extOf, nameCollator } from "@/lib/utils";

function Empty({ icon: Icon, text, hint }: { icon: typeof ClockIcon; text: string; hint?: string }) {
  return (
    <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
      <Icon className="size-9 stroke-[1.4]" />
      <p>{text}</p>
      {hint && <p className="text-xs">{hint}</p>}
    </div>
  );
}

const crumbs = (label: string) => [{ label: t("Files") }, { label }];

/** Lists the server doesn't sort (search, recent) are sorted in the frontend */
function useClientSort(items: Located[] | undefined, sort: { key: SortKey; order: SortOrder }, enabled: boolean) {
  return useMemo(() => {
    if (!items || !enabled) return items ?? [];
    const dir = sort.order === "asc" ? 1 : -1;
    const val = (n: Located) =>
      sort.key === "size" ? n.size : sort.key === "updated" ? n.updated_at : sort.key === "type" ? (n.kind === "folder" ? "" : extOf(n.name)) : n.name;
    return [...items].sort((a, b) => {
      if (a.kind !== b.kind) return a.kind === "folder" ? -1 : 1;
      const x = val(a);
      const y = val(b);
      const c = typeof x === "number" ? x - (y as number) : nameCollator.compare(String(x), String(y));
      return (c || nameCollator.compare(a.name, b.name)) * dir;
    });
  }, [items, sort.key, sort.order, enabled]);
}

export function RecentPage() {
  // Ordered by time used by default; column sorting applies only after clicking a column header
  const [sort, setSort] = useState<{ key: SortKey; order: SortOrder } | null>(null);
  const q = useQuery({ queryKey: ["recent"], queryFn: api.recent });
  const items = useClientSort(q.data, sort ?? { key: "updated", order: "desc" }, !!sort);
  return (
    <Explorer
      items={items}
      loading={q.isLoading}
      error={q.error}
      showLocation
      sort={sort ?? undefined}
      onSort={(key) => setSort({ key, order: sort?.key === key && sort.order === "asc" ? "desc" : "asc" })}
      crumbs={crumbs(t("Recent"))}
      icon={ClockIcon}
      onSortChange={setSort}
      empty={<Empty icon={ClockIcon} text={t("No recent files yet")} />}
    />
  );
}

export function FavoritesPage() {
  const [sort, onSort, setSort] = useSort();
  const q = useQuery({ queryKey: ["favorites", sort.key, sort.order], queryFn: () => api.favorites(sort.key, sort.order) });
  return (
    <Explorer
      items={q.data ?? []}
      loading={q.isLoading}
      error={q.error}
      showLocation
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      crumbs={crumbs(t("Favorite"))}
      icon={StarIcon}
      empty={<Empty icon={StarIcon} text={t("No favorites yet")} hint={t("Right-click a file, or choose Add to favorites from the ⋯ menu.")} />}
    />
  );
}

export function SearchPage() {
  const [params] = useSearchParams();
  const term = params.get("q") ?? "";
  const [sort, onSort, setSort] = useSort();
  const q = useQuery({ queryKey: ["search", term], queryFn: () => api.search(term), enabled: !!term });
  const items = useClientSort(q.data, sort, true);
  return (
    <Explorer
      items={items}
      loading={q.isLoading}
      error={q.error}
      showLocation
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      crumbs={crumbs(t("Search results for \"{term}\"", { term }))}
      icon={SearchIcon}
      empty={<Empty icon={SearchXIcon} text={t("No matching files found")} />}
    />
  );
}
