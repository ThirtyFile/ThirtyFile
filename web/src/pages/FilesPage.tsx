import { useEffect, useEffectEvent, useMemo } from "react";
import { Navigate, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { FolderIcon, Loader2Icon } from "lucide-react";
import { MovingBanner, OfflineBanner, ReadOnlyBanner } from "@/components/OfflineNotice";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import { Explorer } from "@/components/Explorer";
import { Frame, crumbPath } from "@/components/Frame";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { expandPath, treePathOf } from "@/components/FolderTree";
import { useDrives } from "@/lib/drives";
import { hasPersonal, homeFolder } from "@/lib/home";
import type { GroupBy } from "@/lib/listView";
import { useAllPages } from "@/lib/pages";
import { pathOf } from "@/lib/paths";
import { useMe, usePersisted } from "@/lib/session";
import { useFolderWindows } from "@/lib/windows";
import { t } from "@/lib/i18n";
import { locationOf } from "@/components/frame/location";
import { useSort } from "@/lib/sort";
import { useView } from "@/components/style";
import { groupable } from "@/components/fileList/layout";

/** Folder page: `/files` (My files, or the first space of someone without it), `/files/shared` (All files), `/files/:id` */
export function FilesPage() {
  const { id } = useParams();
  const me = useMe();
  // Someone without "My files" (the alias "root") starts in their first space instead
  if ((!id || id === "root") && !hasPersonal(me)) return <HomeWithoutPersonal />;
  return <FolderPage id={id ?? "root"} />;
}

/** Opens the first space of someone who has no "My files", or says that they have no space yet */
function HomeWithoutPersonal() {
  const me = useMe();
  const drives = useDrives();
  const home = homeFolder(me, drives.data);
  if (home) return <Navigate to={`/files/${home}`} replace />;
  return (
    <Frame toolbar={null} crumbs={[{ label: t("All spaces"), to: "/drives", virtual: true }, { label: t("Files") }]} upTo="/drives" icon={FolderIcon}>
      {home === null ? (
        <EmptyState
          icon={FolderIcon}
          title={t("You don't have any spaces yet")}
          hint={
            me.personal_pending
              ? t('Your "My files" is being set up, and appears here once its storage is available.')
              : t('Ask an administrator for access to a space. Files shared with you are under "Shared with me".')
          }
        />
      ) : drives.error ? (
        <ErrorState message={drives.error.message} onRetry={() => drives.refetch()} />
      ) : (
        <div className="flex flex-1 items-center justify-center text-muted-foreground" role="status" aria-label={t("Loading…")}>
          <Loader2Icon className="size-5 animate-spin" />
        </div>
      )}
    </Frame>
  );
}

function FolderPage({ id }: { id: string }) {
  const [sort, onSort, setSort] = useSort();
  // Refresh more often while the storage service is offline, so the notice disappears automatically once it recovers
  const info = useQuery({
    queryKey: keys.node(id),
    queryFn: () => api.node(id),
    refetchInterval: (q) => (q.state.data?.offline ? 15_000 : 60_000),
  });
  const node = info.data?.node;
  const folderId = node?.id;
  // A large folder loads the parts in view (lib/windows). Grouped by date or type, every group shows in full, so the
  // whole folder loads, a page after another. The Columns and Gallery views don't group: they load the parts in view too
  const [groupBy] = usePersisted<GroupBy>("tf-group", "none");
  const [view] = useView();
  const grouped = groupBy !== "none" && groupable(view);
  const windows = useFolderWindows(folderId, sort.key, sort.order, !!folderId && !grouped);
  const pages = useAllPages(
    keys.childrenPages(folderId, sort.key, sort.order),
    (limit, after, signal) => api.childrenPage(folderId!, sort.key, sort.order, limit, after, signal),
    !!folderId && grouped,
  );
  const children = grouped
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
  const path = info.data?.path ?? [];
  const loc = locationOf(info.data);

  // Expand the left-hand tree down to the current folder
  const expandHere = useEffectEvent(() => {
    const ids = info.data && treePathOf(info.data);
    if (ids) expandPath(ids);
  });
  const pathKey = path.map((c) => c.id).join();
  useEffect(() => expandHere(), [pathKey, node?.id]);

  // The Columns view's columns: from the top of the space (or the folder shared with this person) down to this folder
  const data = info.data;
  const trail = useMemo(() => (data ? (data.via_share ? data.path : [{ id: data.drive.root_id, name: loc.rootLabel }, ...data.path]) : null), [data, loc.rootLabel]);

  const parent = path.length >= 2 ? `/files/${path[path.length - 2].id}` : path.length === 1 ? loc.rootUrl : "/drives";

  return (
    <Explorer
      notice={info.data?.offline ? <OfflineBanner reason={info.data.offline} /> : info.data?.moving ? <MovingBanner /> : info.data?.read_only ? <ReadOnlyBanner /> : undefined}
      readOnly={info.data?.read_only}
      offline={info.data?.offline}
      items={children.items}
      list={children.list}
      loading={info.isLoading || children.isLoading}
      loadingMore={children.loadingMore}
      error={info.error ?? children.error}
      partError={children.partError}
      onRetryPart={children.retry}
      folderId={folderId}
      spaceId={info.data && !info.data.via_share ? info.data.drive.id : undefined}
      role={info.data?.role}
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      upTo={parent}
      showOwner={!!info.data && (info.data.drive.kind !== "personal" || info.data.via_share)}
      folder={node}
      trail={trail}
      icon={loc.icon}
      crumbs={loc.crumbs}
      path={pathOf(info.data) ?? crumbPath(loc.crumbs)}
      emptyHint={info.data?.drive.kind === "company" ? t("This space is shared with the whole company. Everyone can see the files uploaded here.") : undefined}
    />
  );
}
