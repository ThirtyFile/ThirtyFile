import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, ExternalLinkIcon, FolderOpenIcon, HistoryIcon, Link2Icon, Link2OffIcon, PencilIcon, RefreshCwIcon, Trash2Icon } from "lucide-react";
import { useNavigate } from "react-router";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type ShareFilter, type ShareInfo } from "@/api";
import { FileIcon } from "@/components/FileIcon";
import { Frame, ToolButton, ToolSeparator, type FrameProps } from "@/components/Frame";
import { EditShareDialog, LinksOffNotice, LocalLinkWarning, deleteLinkQuestion, sharePath, shareSpace, useShareLink } from "@/components/ShareDialog";
import { confirm } from "@/components/confirm";
import { copyAndSay, formatDate, formatDateTime } from "@/lib/utils";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { useMe } from "@/lib/session";
import { t, tc } from "@/lib/i18n";

/** "My share links": the caller's own links, or every link they may manage (space managers, item owners) */
export function SharesPage() {
  const [scope, setScope] = useState<"mine" | "managed">("mine");
  return (
    <ShareLinks
      filter={{ scope }}
      frame={{ crumbs: [{ label: t("Files") }, { label: t("My share links") }], icon: Link2Icon }}
      showOwner={scope === "managed"}
      extraToolbar={
        <>
          <ToolSeparator />
          <select
            className="h-7 rounded-md border bg-background px-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
            aria-label={t("Show")}
            value={scope}
            onChange={(e) => setScope(e.target.value as "mine" | "managed")}
          >
            <option value="mine">{t("Links I created")}</option>
            <option value="managed">{t("All links I can manage")}</option>
          </select>
        </>
      }
      empty={
        scope === "mine" ? (
          <EmptyState icon={Link2OffIcon} title={t("You haven't shared any files yet")} hint={t("Select a file and click Create share link on the toolbar to create a public link.")} />
        ) : (
          <EmptyState icon={Link2OffIcon} title={t("No share links")} hint={t("Links that others create in spaces you manage, or on your files, appear here.")} />
        )
      }
    />
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
  const q = useQuery({ queryKey: ["shares", "list", filter], queryFn: () => api.shares(undefined, filter) });
  const [selected, setSelected] = useState<string | null>(null);
  const [editing, setEditing] = useState<ShareInfo | null>(null);
  const { link: shareLink, local, admin } = useShareLink();
  // Access log dialog: a specific link, or "all" for all my links
  const [accessOf, setAccessOf] = useState<string | null>(null);
  const items = q.data ?? [];
  const current = items.find((s) => s.id === selected);
  // The item can only be opened by people who can reach it; administrators can't open personal spaces
  const canOpenItem = (s: ShareInfo) => s.owner_id === me.id || !(admin && s.drive_kind === "personal" && s.drive_owner !== me.username);

  const remove = useMutation({
    mutationFn: api.deleteShare,
    onSuccess: () => {
      toast.success(t("Share link deleted"));
      setSelected(null);
      qc.invalidateQueries({ queryKey: ["shares"] });
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
      <ToolButton icon={CopyIcon} label={t("Copy link")} showLabel disabled={!current} onClick={() => current && copy(current.id)} />
      <ToolButton
        icon={ExternalLinkIcon}
        label={t("Open")}
        showLabel
        disabled={!current}
        onClick={() => current && window.open(sharePath(current.id), "_blank", "noopener")}
      />
      <ToolButton icon={PencilIcon} label={t("Edit")} showLabel disabled={!current} onClick={() => current && setEditing(current)} />
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
      cell: (s) => (
        <div className="flex max-w-[360px] items-center gap-[7px]">
          <FileIcon node={{ kind: s.node_kind, name: s.node_name, mime: "" }} className="size-4" />
          <span className="truncate">{s.node_name}</span>
        </div>
      ),
    },
    ...(showOwner
      ? ([
          { header: t("Space"), className: "max-md:hidden", cellClassName: "text-muted-foreground", cell: (s) => shareSpace(s) },
          { header: t("Created by"), cellClassName: "text-muted-foreground", cell: (s) => s.owner_name },
        ] as Column<ShareInfo>[])
      : []),
    {
      header: t("Link"),
      cellClassName: "font-mono text-muted-foreground",
      cell: (s) => sharePath(s.id),
    },
    {
      header: t("Date created"),
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) => formatDateTime(s.created_at),
    },
    {
      header: t("Expires"),
      className: "max-md:hidden",
      cellClassName: "text-muted-foreground",
      cell: (s) =>
        s.expires_at ? (s.expires_at * 1000 <= Date.now() ? t("Expired {date}", { date: formatDate(s.expires_at) }) : formatDate(s.expires_at)) : t("Never expires"),
    },
    {
      header: t("Downloads"),
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
      cellClassName: "text-muted-foreground tabular-nums",
      cell: (s) => s.views,
    },
    {
      header: t("Last accessed"),
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
        selectedKey={selected}
        onSelect={setSelected}
        onOpen={(s) => copy(s.id)}
        empty={empty}
        menu={() =>
          current ? (
            <>
              <DropdownMenuItem onClick={() => copy(current.id)}>
                <CopyIcon /> {t("Copy link")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => window.open(sharePath(current.id), "_blank", "noopener")}>
                <ExternalLinkIcon /> {t("Open share page")}
              </DropdownMenuItem>
              {canOpenItem(current) && (
                <DropdownMenuItem onClick={() => navigate(current.node_kind === "folder" ? `/files/${current.node_id}` : `/view/${current.node_id}`)}>
                  <FolderOpenIcon /> {current.node_kind === "folder" ? t("Open folder") : t("Open file")}
                </DropdownMenuItem>
              )}
              <DropdownMenuItem onClick={() => setEditing(current)}>
                <PencilIcon /> {t("Edit link")}
              </DropdownMenuItem>
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
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: ["shares"] })}>
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
                  : t("Access log: {name}", { name: items.find((s) => s.id === accessOf)?.node_name ?? sharePath(accessOf) })}
              </DialogTitle>
            </DialogHeader>
            <ShareAccessLog shareId={accessOf === "all" ? undefined : accessOf} className="min-h-0 flex-1" />
          </DialogContent>
        </Dialog>
      )}
    </Frame>
  );
}
