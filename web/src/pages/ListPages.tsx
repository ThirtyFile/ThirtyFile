import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { ClockIcon, FolderSearchIcon, PencilIcon, SearchIcon, SearchXIcon, StarIcon, TagIcon, Trash2Icon } from "lucide-react";
import { api, type Located, type SearchFilter, type SortKey, type SortOrder, type TagColor } from "@/api";
import { keys } from "@/api/queryKeys";
import { Explorer } from "@/components/Explorer";
import { TagDot, askToDeleteTag, recolor } from "@/components/tags";
import { Button } from "@/components/ui/button";
import { TAG_COLORS, editTag, useTags } from "@/lib/tags";
import { DAY, SEARCH_DATES, SEARCH_SIZES, SEARCH_TYPES } from "@/lib/searchFilters";
import { editSmartFolder, queryFromSearch, smartPath } from "@/lib/smartFolders";
import { useSort } from "@/lib/sort";
import { t } from "@/lib/i18n";
import { extOf, nameCollator } from "@/lib/utils";
import { NativeSelect } from "@/components/ui/native-select";

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
            : sort.key === "type"
              ? n.kind === "folder"
                ? ""
                : extOf(n.name)
              : n.name;
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
  const q = useQuery({ queryKey: keys.recent(), queryFn: api.recent });
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
  const q = useQuery({ queryKey: keys.favorites(sort.key, sort.order), queryFn: () => api.favorites(sort.key, sort.order) });
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

/** The items with one of the person's tags (from the navigation pane), with the tag's name, colour and deleting it */
export function TaggedPage() {
  const id = Number(useParams().id);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { byId, loading } = useTags();
  const tag = byId.get(id);
  const [sort, onSort, setSort] = useSort();
  const q = useQuery({ queryKey: keys.tagged(id, sort.key, sort.order), queryFn: () => api.tagged(id, sort.key, sort.order), enabled: !!tag });
  const name = tag?.name ?? (loading ? "…" : t("Tag not found"));
  const bar = tag && (
    <div className="flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs text-muted-foreground" role="group" aria-label={t("Tag")}>
      <TagDot color={tag.color} />
      <span className="font-medium text-foreground">{tag.name}</span>
      <Button variant="ghost" size="xs" onClick={() => void editTag(tag)}>
        <PencilIcon /> {t("Rename…")}
      </Button>
      <label className="flex items-center gap-1.5">
        {t("Color")}
        <NativeSelect size="xs" value={tag.color} onChange={(e) => void recolor(qc, tag, e.target.value as TagColor)}>
          {TAG_COLORS.map((c) => (
            <option key={c.id} value={c.id}>
              {c.label()}
            </option>
          ))}
        </NativeSelect>
      </label>
      <Button variant="ghost" size="xs" onClick={() => void askToDeleteTag(qc, tag, () => navigate("/files"))}>
        <Trash2Icon /> {t("Delete tag")}
      </Button>
      {q.data?.truncated && <span role="status">{t("Showing the first {n} items.", { n: q.data.items.length })}</span>}
    </div>
  );
  return (
    <Explorer
      items={q.data?.items ?? []}
      loading={loading || q.isLoading}
      error={q.error}
      showLocation
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      notice={bar}
      crumbs={[{ label: t("Files") }, { label: t("Tags") }, { label: name }]}
      icon={TagIcon}
      empty={
        tag ? (
          <Empty icon={TagIcon} text={t("Nothing has this tag yet")} hint={t("Right-click files or folders and choose Tags to put it on them.")} />
        ) : (
          <Empty icon={TagIcon} text={loading ? "…" : t("Tag not found")} />
        )
      }
    />
  );
}

export function SearchPage() {
  const [params, setParams] = useSearchParams();
  const navigate = useNavigate();
  const term = params.get("q") ?? "";
  const within = params.get("in") ?? undefined;
  const [type, date, size, tag] = [params.get("type") ?? "", params.get("date") ?? "", params.get("size") ?? "", params.get("tag") ?? ""];
  const { tags } = useTags();
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
  const folder = useQuery({ queryKey: keys.node(within), queryFn: () => api.node(within!), enabled: !!within });
  const filter: SearchFilter = {
    in: within,
    ...SEARCH_TYPES.find((x) => x.id === type)?.filter,
    ...SEARCH_SIZES.find((x) => x.id === size)?.filter,
    tags: tag || undefined,
  };
  const days = SEARCH_DATES.find((x) => x.id === date)?.days;
  // Rounded to the hour, so the query key stays the same while the page is open
  if (days) filter.from = Math.floor(Date.now() / 3_600_000) * 3600 - days * DAY;
  const [sort, onSort, setSort] = useSort();
  const q = useQuery({ queryKey: keys.search(term, filter), queryFn: () => api.search(term, filter), enabled: !!term });
  const items = useClientSort(q.data?.items, sort, true);
  const folderName = (folder.data?.is_root ? folder.data.drive.name : folder.data?.node.name) ?? "";
  const filters = (
    <div className="flex flex-wrap items-center gap-2 border-b px-3 py-1.5 text-xs text-muted-foreground" role="group" aria-label={t("Search filters")}>
      <label className="flex items-center gap-1.5">
        {t("Look in")}
        <NativeSelect size="xs" value={within ?? ""} onChange={(e) => set("in", e.target.value)}>
          {within && <option value={within}>{t("{name} and its subfolders", { name: folderName || "…" })}</option>}
          <option value="">{t("Everywhere I have access")}</option>
        </NativeSelect>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Type")}
        <NativeSelect size="xs" value={type} onChange={(e) => set("type", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_TYPES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </NativeSelect>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Date modified")}
        <NativeSelect size="xs" value={date} onChange={(e) => set("date", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_DATES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </NativeSelect>
      </label>
      <label className="flex items-center gap-1.5">
        {t("Size")}
        <NativeSelect size="xs" value={size} onChange={(e) => set("size", e.target.value)}>
          <option value="">{t("Any")}</option>
          {SEARCH_SIZES.map((x) => (
            <option key={x.id} value={x.id}>
              {x.label()}
            </option>
          ))}
        </NativeSelect>
      </label>
      {(tags.length > 0 || tag) && (
        <label className="flex items-center gap-1.5">
          {t("Tag")}
          <NativeSelect size="xs" value={tag} onChange={(e) => set("tag", e.target.value)}>
            <option value="">{t("Any")}</option>
            {tags.map((x) => (
              <option key={x.id} value={String(x.id)}>
                {x.name}
              </option>
            ))}
          </NativeSelect>
        </label>
      )}
      {q.data?.truncated && <span role="status">{t("Showing the first {n} results. Add words or filters to find the rest.", { n: q.data.items.length })}</span>}
      <Button
        variant="ghost"
        size="xs"
        className="ml-auto"
        disabled={!term.trim() && !tag}
        onClick={async () => {
          const query = queryFromSearch({ term, within, type, date, size, tag });
          const saved = await editSmartFolder(undefined, { name: term.trim() || (tags.find((x) => String(x.id) === tag)?.name ?? ""), query });
          if (saved) navigate(smartPath(saved));
        }}
      >
        <FolderSearchIcon /> {t("Save as smart folder")}
      </Button>
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
      // A new search from here looks where this one does
      searchPlaceholder={within ? t("Search {name}", { name: folderName || "…" }) : undefined}
      crumbs={crumbs(t('Search results for "{term}"', { term }))}
      icon={SearchIcon}
      empty={<Empty icon={SearchXIcon} text={t("No matching files found")} />}
    />
  );
}
