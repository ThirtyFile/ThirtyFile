import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CopyIcon, ExternalLinkIcon, FolderOpenIcon, HistoryIcon, Link2Icon, Link2OffIcon, RefreshCwIcon } from "lucide-react";
import { useNavigate } from "react-router";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { DataTable, EmptyState, type Column } from "@/components/DataTable";
import { toast } from "sonner";
import { api, type ShareInfo } from "@/api";
import { FileIcon } from "@/components/FileIcon";
import { Frame, ToolButton } from "@/components/Frame";
import { LocalLinkWarning, sharePath, useShareLink } from "@/components/ShareDialog";
import { copyText, formatDate, formatDateTime } from "@/lib/utils";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ShareAccessLog } from "@/components/logs/ShareAccessLog";
import { t, tc } from "@/lib/i18n";

export function SharesPage() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: ["shares"], queryFn: () => api.shares() });
  const [selected, setSelected] = useState<string | null>(null);
  const { link: shareLink, local, admin } = useShareLink();
  // Access log dialog: a specific link, or "all" for all my links
  const [accessOf, setAccessOf] = useState<string | null>(null);
  const items = q.data ?? [];
  const current = items.find((s) => s.id === selected);

  const remove = useMutation({
    mutationFn: api.deleteShare,
    onSuccess: () => {
      toast.success(t("Share link disabled"));
      setSelected(null);
      qc.invalidateQueries({ queryKey: ["shares"] });
    },
    onError: (e) => toast.error(e.message),
  });

  const copy = async (id: string) => {
    await copyText(shareLink(id));
    toast.success(t("Link copied"));
  };

  const toolbar = (
    <>
      <ToolButton icon={CopyIcon} label={t("Copy link")} showLabel disabled={!current} onClick={() => current && copy(current.id)} />
      <ToolButton
        icon={ExternalLinkIcon}
        label={t("Open")}
        showLabel
        disabled={!current}
        onClick={() => current && window.open(sharePath(current.id), "_blank")}
      />
      <ToolButton icon={Link2OffIcon} label={t("Disable link")} showLabel disabled={!current} onClick={() => current && remove.mutate(current.id)} />
      <span className="mx-1 h-5 border-l" />
      <ToolButton
        icon={HistoryIcon}
        label={current ? t("Access log") : t("All access logs")}
        showLabel
        onClick={() => setAccessOf(current ? current.id : "all")}
      />
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
      cell: (s) => (s.expires_at ? formatDate(s.expires_at) : t("Never expires")),
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
    <Frame toolbar={toolbar} crumbs={[{ label: t("Files") }, { label: t("My shares") }]} icon={Link2Icon} footer={<span>{t("{n} share link|{n} share links", { n: items.length })}</span>}>
      {local && (
        <div className="shrink-0 border-b p-2">
          <LocalLinkWarning admin={admin} />
        </div>
      )}
      <DataTable
        rows={items}
        rowKey={(s) => s.id}
        columns={columns}
        compact
        loading={q.isLoading}
        selectedKey={selected}
        onSelect={setSelected}
        onOpen={(s) => copy(s.id)}
        empty={<EmptyState icon={Link2OffIcon} title={t("You haven't shared any files yet")} hint={t("Select a file and click Share on the toolbar to create a public link.")} />}
        menu={() =>
          current ? (
            <>
              <DropdownMenuItem onClick={() => copy(current.id)}>
                <CopyIcon /> {t("Copy link")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => window.open(sharePath(current.id), "_blank")}>
                <ExternalLinkIcon /> {t("Open share page")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => navigate(current.node_kind === "folder" ? `/files/${current.node_id}` : `/view/${current.node_id}`)}>
                <FolderOpenIcon /> {current.node_kind === "folder" ? t("Open folder") : t("Open file")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setAccessOf(current.id)}>
                <HistoryIcon /> {t("Access log")}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={() => remove.mutate(current.id)}>
                <Link2OffIcon /> {t("Disable link")}
              </DropdownMenuItem>
            </>
          ) : (
            <>
              <DropdownMenuItem onClick={() => setAccessOf("all")}>
                <HistoryIcon /> {t("All access logs")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => qc.invalidateQueries({ queryKey: ["shares"] })}>
                <RefreshCwIcon /> {t("Refresh")}
              </DropdownMenuItem>
            </>
          )
        }
      />
      {accessOf && (
        <Dialog open onOpenChange={(o) => !o && setAccessOf(null)}>
          <DialogContent className="flex h-[75vh] flex-col gap-0 p-0 sm:max-w-4xl">
            <DialogHeader className="border-b px-4 py-3">
              <DialogTitle>
                {accessOf === "all"
                  ? t("Access log for my share links")
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
