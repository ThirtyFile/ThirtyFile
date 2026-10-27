import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, ExternalLinkIcon, FolderOpenIcon, HistoryIcon, PencilIcon, RefreshCwIcon, Trash2Icon } from "lucide-react";
import { Link, useNavigate } from "react-router";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type ShareFilter, type ShareInfo } from "@/api";
import { keys } from "@/api/queryKeys";
import { FileIcon } from "@/components/FileIcon";
import { Frame, ToolButton, ToolSeparator, type FrameProps } from "@/components/Frame";
import { EditShareDialog, LinksOffNotice, LocalLinkWarning, deleteLinkQuestion, sharePath, useShareLink } from "@/components/ShareDialog";
import { SHARE_SORT_KEYS, type ShareSort, type ShareSortKey, sortShares, shareLocation, sharePathOfItem } from "@/components/shareList";
import { confirm } from "@/lib/confirm";
import { copyAndSay, formatDate, formatDateTime } from "@/lib/utils";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { useMe, usePersisted } from "@/lib/session";
import { t, tc } from "@/lib/i18n";

/** What a link in someone else's personal space shows instead of its item's name (administrators only delete them) */
const privateItem = () => t("In someone else's personal space");

const linkName = (s?: ShareInfo) => (s?.private ? privateItem() : s?.node_name);

/** The place of a link's item. It is cut at the front when it is long, so the folder and the one above it stay in view, and opens the folder when it may be
 * opened. `bdi` keeps the order of the names right in a text that is laid out from the right */
function Location({ share, text }: { share: ShareInfo; text: string }) {
  const name = (
    <span className="block max-w-[320px] truncate text-left [direction:rtl]">
      <bdi>{text}</bdi>
    </span>
  );
  if (!share.location_id || share.private) return name;
  return (
    <Link to={`/files/${share.location_id}`} className="hover:text-foreground hover:underline" onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
      {name}
    </Link>
  );
}

/** A table of share links with copy, open, edit, delete and the access log */
export function ShareLinks({
  filter,
  frame,
  showOwner,
  extraToolbar,
  empty,
}: {
  filter: ShareFilter;
  frame: Omit<FrameProps, "toolbar" | "footer" | "children">;
  /** Show who created each link and in which space it is */
  showOwner: boolean;
  extraToolbar?: React.ReactNode;
  empty: React.ReactNode;
}) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const me = useMe();
  const q = useQuery({ queryKey: keys.shareList(filter), queryFn: () => api.shares(undefined, filter) });
  const [selected, setSelected] = useState<string | null>(null);
  const [editing, setEditing] = useState<ShareInfo | null>(null);
  const { link: shareLink, local, admin } = useShareLink();
  // Access log dialog: a specific link, or "all" for all my links
  const [accessOf, setAccessOf] = useState<string | null>(null);
  // The order of the table, kept for the next visit (newest link first until a header is clicked)
  const [sort, setSort] = usePersisted<ShareSort>("tf-share-sort", { key: "created", order: "desc" }, (v) => SHARE_SORT_KEYS.includes(v.key) && (v.order === "asc" || v.order === "desc"));
  const onSort = (key: string) => setSort({ key: key as ShareSortKey, order: sort.key === key && sort.order === "asc" ? "desc" : "asc" });
  const items = useMemo(() => sortShares(q.data ?? [], sort, me), [q.data, sort, me]);
  const current = items.find((s) => s.id === selected);
  // A link in someone else's personal space, listed for an administrator: it can only be deleted
  const usable = current && !current.private ? current : undefined;
  // The item can only be opened by people who can reach it; administrators can't open personal spaces
  const canOpenItem = (s: ShareInfo) => s.owner_id === me.id || !(admin && s.drive_kind === "personal" && s.drive_owner !== me.username);

  const remove = useMutation({
    mutationFn: api.deleteShare,
    onSuccess: () => {
      toast.success(t("Share link deleted"));
      setSelected(null);
      void qc.invalidateQueries({ queryKey: keys.shares() });
    },
    onError: (e) => toast.error(e.message),
  });

  const askRemove = async (id: string) => {
    if (await confirm(deleteLinkQuestion())) remove.mutate(id);
  };

  const copy = async (id: string) => {
    await copyAndSay(shareLink(id), t("Link copied"));
  };

  const toolbar = (
    <>
      <ToolButton icon={CopyIcon} label={t("Copy link")} showLabel disabled={!usable} onClick={() => usable && copy(usable.id)} />
      <ToolButton icon={ExternalLinkIcon} label={t("Open")} showLabel disabled={!usable} onClick={() => usable && window.open(sharePath(usable.id), "_blank", "noopener")} />
      <ToolButton icon={PencilIcon} label={t("Edit")} showLabel disabled={!usable} onClick={() => usable && setEditing(usable)} />
      <ToolButton icon={Trash2Icon} label={t("Delete link")} showLabel disabled={!current} onClick={() => current && askRemove(current.id)} />
      <ToolSeparator />
      <ToolButton
        icon={HistoryIcon}
        label={current ? t("Access log") : t("All access logs")}
        showLabel
        disabled={!current && showOwner && !admin}
        onClick={() => setAccessOf(current ? current.id : "all")}
      />
      {extraToolbar}
    </>
  );

  const columns: Column<ShareInfo>[] = [
    {
      header: t("Name"),
      sortKey: "name",
      cell: (s) => (
        <div className="flex max-w-[360px] items-center gap-[7px]">
          <FileIcon node={{ kind: s.node_kind, name: s.node_name, mime: "" }} className="size-4" />
          {s.private ? <span className="truncate text-muted-foreground italic">{privateItem()}</span> : <span className="truncate">{s.node_name}</span>}
        </div>
      ),
    },
    {
      header: t("Location"),
      sortKey: "location",
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      title: (s) => sharePathOfItem(s, me),
      cell: (s) => <Location share={s} text={shareLocation(s, me)} />,
    },
    ...(showOwner ? ([{ header: t("Created by"), sortKey: "owner", cellClassName: "text-muted-foreground", cell: (s) => s.owner_name }] as Column<ShareInfo>[]) : []),
    {
      header: t("Link"),
      cellClassName: "font-mono text-muted-foreground",
      cell: (s) => (s.private ? "—" : sharePath(s.id)),
    },
    {
      header: t("Date created"),
      sortKey: "created",
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => formatDateTime(s.created_at),
    },
    {
      header: t("Expires"),
      sortKey: "expires",
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => (s.expires_at ? (s.expires_at * 1000 <= Date.now() ? t("Expired {date}", { date: formatDate(s.expires_at) }) : formatDate(s.expires_at)) : t("Never expires")),
    },
    {
      header: t("Downloads"),
      sortKey: "downloads",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (s) => `${s.downloads}${s.max_downloads ? ` / ${s.max_downloads}` : ""}`,
    },
    {
      header: t("Password"),
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => (s.has_password ? tc("has", "Yes") : "—"),
    },
    {
      header: t("Views"),
      sortKey: "views",
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (s) => s.views,
    },
    {
      header: t("Last accessed"),
      sortKey: "accessed",
      className: "max-lg:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => (s.last_access ? formatDateTime(s.last_access) : t("Not accessed yet")),
    },
  ];

  return (
    <Frame {...frame} toolbar={toolbar} footer={<span>{t("{n} share link|{n} share links", { n: items.length })}</span>}>
      {(local || !me.share_policy.public_links) && (
        <div className="grid shrink-0 gap-2 border-b p-2">
          {local && <LocalLinkWarning admin={admin} />}
          {!me.share_policy.public_links && <LinksOffNotice />}
        </div>
      )}
      <DataTable
        label={t("Share links")}
        rows={items}
        rowKey={(s) => s.id}
        columns={columns}
        compact
        loading={q.isLoading}
        error={q.error}
        onRetry={() => q.refetch()}
        sort={sort}
        onSort={onSort}
        selectedKey={selected}
        onSelect={setSelected}
        onOpen={(s) => !s.private && copy(s.id)}
        empty={empty}
        menu={() =>
          current ? (
            <>
              {usable && (
                <>
                  <DropdownMenuItem onClick={() => copy(usable.id)}>
                    <CopyIcon /> {t("Copy link")}
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => window.open(sharePath(usable.id), "_blank", "noopener")}>
                    <ExternalLinkIcon /> {t("Open share page")}
                  </DropdownMenuItem>
                </>
              )}
              {usable && canOpenItem(usable) && (
                <DropdownMenuItem onClick={() => navigate(current.node_kind === "folder" ? `/files/${current.node_id}` : `/view/${current.node_id}`)}>
                  <FolderOpenIcon /> {current.node_kind === "folder" ? t("Open folder") : t("Open file")}
                </DropdownMenuItem>
              )}
              {usable && canOpenItem(usable) && usable.location_id && (
                <DropdownMenuItem onClick={() => navigate(`/files/${usable.location_id}`)}>
                  <FolderOpenIcon /> {t("Open file location")}
                </DropdownMenuItem>
              )}
              {usable && (
                <DropdownMenuItem onClick={() => setEditing(usable)}>
                  <PencilIcon /> {t("Edit link")}
                </DropdownMenuItem>
              )}
              <DropdownMenuItem onClick={() => setAccessOf(current.id)}>
                <HistoryIcon /> {t("Access log")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={() => askRemove(current.id)}>
                <Trash2Icon /> {t("Delete link")}
              </DropdownMenuItem>
            </>
          ) : (
            <>
              {!(showOwner && !admin) && (
                <DropdownMenuItem onClick={() => setAccessOf("all")}>
                  <HistoryIcon /> {t("All access logs")}
                </DropdownMenuItem>
              )}
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: keys.shares() })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {editing && <EditShareDialog share={editing} onClose={() => setEditing(null)} />}
      {accessOf && (
        <Dialog open onOpenChange={(o) => !o && setAccessOf(null)}>
          <DialogContent className="flex h-[75vh] flex-col gap-0 p-0 sm:max-w-4xl">
            <DialogHeader className="border-b px-4 py-3">
              <DialogTitle>
                {accessOf === "all"
                  ? admin && showOwner
                    ? t("Access log for all share links")
                    : t("Access log for my share links")
                  : t("Access log: {name}", { name: linkName(items.find((s) => s.id === accessOf)) ?? sharePath(accessOf) })}
              </DialogTitle>
            </DialogHeader>
            <ShareAccessLog shareId={accessOf === "all" ? undefined : accessOf} className="min-h-0 flex-1" />
          </DialogContent>
        </Dialog>
      )}
    </Frame>
  );
}
