import { useEffect, useEffectEvent } from "react";
import { useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { UsersRoundIcon } from "lucide-react";
import { OfflineBanner, ReadOnlyBanner } from "@/components/OfflineNotice";
import { SORT_KEYS, api, type NodeInfo, type SortKey, type SortOrder } from "@/api";
import { Explorer } from "@/components/Explorer";
import { crumbPath, type Crumb } from "@/components/Frame";
import { expandPath } from "@/components/FolderTree";
import { DRIVE_ICON } from "@/lib/drives";
import { useAllPages } from "@/lib/pages";
import { usePersisted } from "@/lib/session";
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

/** Folder page: `/files` (My files), `/files/shared` (All files), `/files/:id` */
export function FilesPage() {
  const { id = "root" } = useParams();
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
    (limit, after) => api.childrenPage(folderId!, sort.key, sort.order, limit, after),
    !!folderId,
  );
  const path = info.data?.path ?? [];
  const loc = locationOf(info.data);

  // Expand the left-hand tree down to the current folder
  const expandHere = useEffectEvent(() => {
    if (!info.data || info.data.via_share) return;
    expandPath(["this-pc", info.data.drive.root_id, ...path.slice(0, -1).map((c) => c.id)]);
  });
  const pathKey = path.map((c) => c.id).join();
  useEffect(() => expandHere(), [pathKey, node?.id]);

  const parent = path.length >= 2 ? `/files/${path[path.length - 2].id}` : path.length === 1 ? loc.rootUrl : "/drives";

  return (
    <Explorer
      notice={
        info.data?.offline ? (
          <OfflineBanner reason={info.data.offline} />
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
      role={info.data?.role}
      sort={sort}
      onSort={onSort}
      onSortChange={setSort}
      upTo={parent}
      showOwner={!!info.data && (info.data.drive.kind !== "personal" || info.data.via_share)}
      folder={node}
      icon={loc.icon}
      crumbs={loc.crumbs}
      path={crumbPath(loc.crumbs)}
      emptyHint={info.data?.drive.kind === "company" ? t("This space is shared with the whole company. Everyone can see the files uploaded here.") : undefined}
    />
  );
}
