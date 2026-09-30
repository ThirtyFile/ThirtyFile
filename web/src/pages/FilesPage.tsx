import { useEffect, useEffectEvent } from "react";
import { Navigate, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { FolderIcon, Loader2Icon, UsersRoundIcon } from "lucide-react";
import { MovingBanner, OfflineBanner, ReadOnlyBanner } from "@/components/OfflineNotice";
import { SORT_KEYS, api, type NodeInfo, type SortKey, type SortOrder } from "@/api";
import { Explorer } from "@/components/Explorer";
import { Frame, crumbPath, type Crumb } from "@/components/Frame";
import { EmptyState } from "@/components/DataTable";
import { ErrorState } from "@/components/ErrorState";
import { expandPath, treePathOf } from "@/components/FolderTree";
import { DRIVE_ICON, useDrives } from "@/lib/drives";
import { hasPersonal, homeFolder } from "@/lib/home";
import { useAllPages } from "@/lib/pages";
import { pathOf } from "@/lib/paths";
import { useMe, usePersisted } from "@/lib/session";
import { t } from "@/lib/i18n";

export function useSort() {
  const [sort, setSort] = usePersisted<{ key: SortKey; order: SortOrder }>(
    "tf-sort",
    { key: "name", order: "asc" },
    (s) => SORT_KEYS.includes(s.key) && (s.order === "asc" || s.order === "desc"),
  );
  const toggle = (key: SortKey) => setSort({ key, order: sort.key === key && sort.order === "asc" ? "desc" : "asc" });
  return [sort, toggle, setSort] as const;
}

/** Build the address bar from node info: All spaces › space › folder…, or Shared with me › shared folder… */
export function locationOf(info: NodeInfo | undefined) {
  if (!info) return { crumbs: [{ label: t("All spaces"), to: "/drives", virtual: true }] as Crumb[], rootUrl: "/drives", rootLabel: "", icon: undefined };
  const via = info.via_share;
  const rootLabel = via ? t("Shared with me") : info.drive.name;
  const rootUrl = via ? "/shared-with-me" : `/files/${info.drive.root_id}`;
  const crumbs: Crumb[] = [
    ...(via ? [] : [{ label: t("All spaces"), to: "/drives", virtual: true }]),
    { label: rootLabel, to: rootUrl },
    ...info.path.map((c) => ({ label: c.name, to: `/files/${c.id}` })),
  ];
  return { crumbs, rootUrl, rootLabel, icon: via ? UsersRoundIcon : DRIVE_ICON[info.drive.kind] };
}

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
              ? t("Your \"My files\" is being set up, and appears here once its storage is available.")
              : t("Ask an administrator for access to a space. Files shared with you are under \"Shared with me\".")
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
    queryKey: ["node", id],
    queryFn: () => api.node(id),
    refetchInterval: (q) => (q.state.data?.offline ? 15_000 : 60_000),
  });
  const node = info.data?.node;
  const folderId = node?.id;
  // Large folders come in pages: the first shows at once, the rest loads in the background
  const children = useAllPages(
    ["children", folderId, sort.key, sort.order],
    (limit, after, signal) => api.childrenPage(folderId!, sort.key, sort.order, limit, after, signal),
    !!folderId,
  );
  const path = info.data?.path ?? [];
  const loc = locationOf(info.data);

  // Expand the left-hand tree down to the current folder
  const expandHere = useEffectEvent(() => {
    const ids = info.data && treePathOf(info.data);
    if (ids) expandPath(ids);
  });
  const pathKey = path.map((c) => c.id).join();
  useEffect(() => expandHere(), [pathKey, node?.id]);

  const parent = path.length >= 2 ? `/files/${path[path.length - 2].id}` : path.length === 1 ? loc.rootUrl : "/drives";

  return (
    <Explorer
      notice={
        info.data?.offline ? (
          <OfflineBanner reason={info.data.offline} />
        ) : info.data?.moving ? (
          <MovingBanner />
        ) : info.data?.read_only ? (
          <ReadOnlyBanner />
        ) : undefined
      }
      readOnly={info.data?.read_only}
      offline={info.data?.offline}
      items={children.items}
      loading={info.isLoading || children.isLoading}
      loadingMore={children.loadingMore}
      error={info.error ?? children.error}
      folderId={folderId}
      spaceId={info.data && !info.data.via_share ? info.data.drive.id : undefined}
      role={info.data?.role}
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      upTo={parent}
      showOwner={!!info.data && (info.data.drive.kind !== "personal" || info.data.via_share)}
      folder={node}
      icon={loc.icon}
      crumbs={loc.crumbs}
      path={pathOf(info.data) ?? crumbPath(loc.crumbs)}
      emptyHint={info.data?.drive.kind === "company" ? t("This space is shared with the whole company. Everyone can see the files uploaded here.") : undefined}
    />
  );
}
