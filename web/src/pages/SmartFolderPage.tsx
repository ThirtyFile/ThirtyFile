import { useMemo } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router";
import { FolderSearchIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { api, type Located } from "@/api";
import { keys } from "@/api/queryKeys";
import { Explorer } from "@/components/Explorer";
import { groupable } from "@/components/fileList/layout";
import { useView } from "@/components/style";
import { QuerySummary, askToDeleteSmartFolder } from "@/components/smartFolders";
import { Button } from "@/components/ui/button";
import { t } from "@/lib/i18n";
import type { GroupBy } from "@/lib/listView";
import { useAllPages } from "@/lib/pages";
import { usePersisted } from "@/lib/session";
import { editSmartFolder, useSmartFolders } from "@/lib/smartFolders";
import { useSort } from "@/lib/sort";
import { useWindows, type WindowSource } from "@/lib/windows";

function Empty({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
      <FolderSearchIcon className="size-9 stroke-[1.4]" />
      <p>{text}</p>
      {hint && <p className="text-xs">{hint}</p>}
    </div>
  );
}

/**
 * A smart folder (`/smart/:id`): what one of the person's saved searches finds now, listed like a folder (a part at a
 * time, in the order chosen), with what it looks for, Edit and Delete above the list. The items have their usual
 * actions; nothing is made or dropped in it, since it holds no items of its own.
 */
export function SmartFolderPage() {
  const id = Number(useParams().id);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { byId, loading } = useSmartFolders();
  const folder = byId.get(id);
  const [sort, onSort, setSort] = useSort();
  // Grouped by date or type, every group shows in full, so the whole list loads, a page after another (as a folder; not
  // in the views that don't group)
  const [groupBy] = usePersisted<GroupBy>("tf-group", "none");
  const [view] = useView();
  const grouped = groupBy !== "none" && groupable(view);
  const source = useMemo<WindowSource<Located>>(
    () => ({
      key: keys.smartAt(id, sort.key, sort.order),
      part: (start, limit, signal) => api.smartItemsAt(id, sort.key, sort.order, start, limit, signal),
      position: async (item) => (await api.smartPosition(id, item, sort.key, sort.order)).position,
    }),
    [id, sort.key, sort.order],
  );
  const windows = useWindows(source, !!folder && !grouped);
  const pages = useAllPages(keys.smartPages(id, sort.key, sort.order), (limit, after, signal) => api.smartItemsPage(id, sort.key, sort.order, limit, after, signal), !!folder && grouped);
  const list = grouped
    ? { items: pages.items, list: undefined, isLoading: pages.isLoading, error: pages.error, loadingMore: pages.loadingMore, partError: null, retry: undefined }
    : {
        items: windows.list.loaded,
        list: windows.list,
        isLoading: windows.isLoading,
        error: windows.error,
        loadingMore: windows.loadingMore,
        partError: windows.partError,
        retry: windows.retry,
      };
  const name = folder?.name ?? (loading ? "…" : t("Smart folder not found"));
  const bar = folder && (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-1.5 text-xs text-muted-foreground" role="group" aria-label={t("Smart folder")}>
      <span className="font-medium text-foreground">{folder.name}</span>
      <QuerySummary query={folder.query} />
      <span className="ml-auto flex items-center gap-1">
        <Button variant="ghost" size="xs" onClick={() => void editSmartFolder(folder)}>
          <PencilIcon /> {t("Edit…")}
        </Button>
        <Button variant="ghost" size="xs" onClick={() => void askToDeleteSmartFolder(qc, folder, () => navigate("/files"))}>
          <Trash2Icon /> {t("Delete smart folder")}
        </Button>
      </span>
    </div>
  );
  return (
    <Explorer
      items={list.items}
      list={list.list}
      loading={loading || list.isLoading}
      loadingMore={list.loadingMore}
      error={list.error}
      partError={list.partError}
      onRetryPart={list.retry}
      smartFolder={folder ? id : undefined}
      showLocation
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      notice={bar}
      crumbs={[{ label: t("Files") }, { label: t("Smart folders") }, { label: name }]}
      icon={FolderSearchIcon}
      empty={
        folder ? (
          <Empty text={t("Nothing matches yet")} hint={t("Items that match show up here as they are added or changed.")} />
        ) : (
          <Empty text={loading ? "…" : t("Smart folder not found")} />
        )
      }
    />
  );
}
