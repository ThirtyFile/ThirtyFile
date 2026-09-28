import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { ClockIcon, SearchIcon, SearchXIcon, StarIcon } from "lucide-react";
import { api, type Located, type SearchFilter, type SortKey, type SortOrder } from "@/api";
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
      sort.key === "size"
        ? n.size
        : sort.key === "updated"
          ? n.updated_at
          : sort.key === "created"
            ? n.created_at
            : sort.key === "type" ? (n.kind === "folder" ? "" : extOf(n.name)) : n.name;
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
      empty={<Empty icon={ClockIcon} text={t("No recent files yet")} hint={t("Files you upload, edit or open show up here, also in shared spaces and folders.")} />}
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
      crumbs={crumbs(t("Favorites"))}
      icon={StarIcon}
      empty={<Empty icon={StarIcon} text={t("No favorites yet")} hint={t("Right-click a file, or choose Add to favorites from the ⋯ menu.")} />}
    />
  );
}

/** Types to filter search results by: extensions, or folders */
const SEARCH_TYPES: { id: string; label: () => string; filter: SearchFilter }[] = [
  { id: "folder", label: () => t("Folders"), filter: { kind: "folder" } },
  { id: "doc", label: () => t("Documents"), filter: { ext: "doc,docx,odt,rtf,pdf,txt,md" } },
  { id: "sheet", label: () => t("Spreadsheets"), filter: { ext: "xls,xlsx,xlsm,ods,csv,tsv" } },
  { id: "slides", label: () => t("Presentations"), filter: { ext: "ppt,pptx,odp" } },
  { id: "image", label: () => t("Pictures"), filter: { ext: "jpg,jpeg,png,gif,webp,bmp,heic,heif,tif,tiff,svg" } },
  { id: "video", label: () => t("Videos"), filter: { ext: "mp4,mov,m4v,mkv,avi,webm,wmv" } },
  { id: "audio", label: () => t("Music and sound"), filter: { ext: "mp3,wav,flac,m4a,aac,ogg,wma" } },
  { id: "archive", label: () => t("Compressed archives"), filter: { ext: "zip,rar,7z,tar,gz" } },
];
const DAY = 86400;
const SEARCH_DATES: { id: string; label: () => string; days: number }[] = [
  { id: "today", label: () => t("Today"), days: 1 },
  { id: "week", label: () => t("Last 7 days"), days: 7 },
  { id: "month", label: () => t("Last 30 days"), days: 30 },
  { id: "year", label: () => t("Last year"), days: 365 },
];
const MB = 1024 * 1024;
const SEARCH_SIZES: { id: string; label: () => string; filter: SearchFilter }[] = [
  { id: "small", label: () => t("Smaller than 1 MB"), filter: { max_size: MB - 1 } },
  { id: "medium", label: () => t("1 to 100 MB"), filter: { min_size: MB, max_size: 100 * MB } },
  { id: "large", label: () => t("Larger than 100 MB"), filter: { min_size: 100 * MB + 1 } },
];
const filterCls = "h-7 rounded-md border bg-background px-1.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring";

export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const term = params.get("q") ?? "";
  const within = params.get("in") ?? undefined;
  const [type, date, size] = [params.get("type") ?? "", params.get("date") ?? "", params.get("size") ?? ""];
  const set = (key: string, value: string) =>
    setParams(
      (p) => {
        const next = new URLSearchParams(p);
        if (value) next.set(key, value);
        else next.delete(key);
        return next;
      },
      { replace: true },
    );
  const folder = useQuery({ queryKey: ["node", within], queryFn: () => api.node(within!), enabled: !!within });
  const filter: SearchFilter = {
    in: within,
    ...SEARCH_TYPES.find((x) => x.id === type)?.filter,
    ...SEARCH_SIZES.find((x) => x.id === size)?.filter,
  };
  const days = SEARCH_DATES.find((x) => x.id === date)?.days;
  // Rounded to the hour, so the query key stays the same while the page is open
  if (days) filter.from = Math.floor(Date.now() / 3_600_000) * 3600 - days * DAY;
  const [sort, onSort, setSort] = useSort();
  const q = useQuery({ queryKey: ["search", term, filter], queryFn: () => api.search(term, filter), enabled: !!term });
  const items = useClientSort(q.data?.items, sort, true);
  const folderName = (folder.data?.is_root ? folder.data.drive.name : folder.data?.node.name) ?? "";
  const filters = (
    <div className="flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs text-muted-foreground" role="group" aria-label={t("Search filters")}>
      <label className="flex items-center gap-1.5">
        {t("Look in")}
        <select className={filterCls} value={within ?? ""} onChange={(e) => set("in", e.target.value)}>
          {within && <option value={within}>{t("{name} and its subfolders", { name: folderName || "…" })}</option>}
          <option value="">{t("Everywhere I have access")}</option>
        </select>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Type")}
        <select className={filterCls} value={type} onChange={(e) => set("type", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_TYPES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </select>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Date modified")}
        <select className={filterCls} value={date} onChange={(e) => set("date", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_DATES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </select>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Size")}
        <select className={filterCls} value={size} onChange={(e) => set("size", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_SIZES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </select>
      </label>
      {q.data?.truncated && <span role="status">{t("Showing the first {n} results. Add words or filters to find the rest.", { n: q.data.items.length })}</span>}
    </div>
  );
  return (
    <Explorer
      items={items}
      loading={q.isLoading}
      error={q.error}
      showLocation
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      notice={filters}
      crumbs={crumbs(t("Search results for \"{term}\"", { term }))}
      icon={SearchIcon}
      empty={<Empty icon={SearchXIcon} text={t("No matching files found")} />}
    />
  );
}
