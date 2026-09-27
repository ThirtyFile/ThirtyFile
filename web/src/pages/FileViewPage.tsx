import { Suspense, lazy, useEffect, useState } from "react";
import { Navigate, useNavigate, useParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  DownloadIcon,
  FileIcon,
  FolderOpenIcon,
  InfoIcon,
  Loader2Icon,
  PanelRightIcon,
  PencilIcon,
  Share2Icon,
  SheetIcon,
  UsersRoundIcon,
} from "lucide-react";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { AccessDialog } from "@/components/AccessDialog";
import { api, privateSource, triggerDownload } from "@/api";
import { Button } from "@/components/ui/button";
import { DetailsPane } from "@/components/DetailsPane";
import { NameDialog } from "@/components/dialogs";
import { OfflinePanel } from "@/components/OfflineNotice";
import { FileViewer } from "@/components/FileViewer";
import { crumbPath, Frame, ToolButton, ToolSeparator } from "@/components/Frame";
import { ShareDialog } from "@/components/ShareDialog";
import { usePersisted, useMe } from "@/lib/session";
import { extOf, formatBytes, formatWinDate } from "@/lib/utils";
import { hasDraft } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { invalidateFiles } from "@/lib/queries";
import { categoryOf, isTextLike, typeLabel } from "@/components/FileIcon";
import { capsOf } from "@/lib/drives";
import { locationOf } from "@/pages/FilesPage";

const SheetEditor = lazy(() => import("@/components/sheet/SheetEditor"));

/** Size limit for saving online edits (same as the server's MAX_EDIT_BYTES) */
const MAX_EDIT_BYTES = 20 * 1024 * 1024;

/** File opened in a tab: `/view/:id` */
export function FileViewPage() {
  const { id = "" } = useParams();
  const me = useMe();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const info = useQuery({
    queryKey: ["node", id],
    queryFn: () => api.node(id),
    // While the storage service is offline, check every 15 seconds and open automatically once it recovers
    refetchInterval: (q) => (q.state.data?.offline ? 15_000 : false),
  });
  const [detailsOpen, setDetailsOpen] = usePersisted("tf-details-pane", false);
  const [dialog, setDialog] = useState<"rename" | "share" | "access" | null>(null);
  // Excel edit mode; when switching back to the tab with unsaved changes, go straight back to the editor
  const [editingId, setEditingId] = useState<string | null>(() => (hasDraft(id) ? id : null));
  // The same route element stays mounted from one file to the next: re-check the draft when the file changes
  useEffect(() => {
    if (hasDraft(id)) setEditingId(id);
  }, [id]);

  const node = info.data?.node;
  // Images, media and unpreviewable files use a custom context menu; text, Word and Excel keep the browser menu so text can be copied
  const customMenu = !!node && ["image", "video", "audio", "other", "archive"].includes(categoryOf(node)) && !isTextLike(node);
  if (node?.kind === "folder") return <Navigate to={`/files/${node.id}`} replace />;

  const path = info.data?.path ?? [];
  const loc = locationOf(info.data);
  const caps = capsOf(info.data?.role, me, info.data?.read_only);
  const canEditSheet = !!node && extOf(node.name) === "xlsx" && caps.write && node.size <= MAX_EDIT_BYTES;
  const sheetEditing = !!node && editingId === node.id && canEditSheet;
  const rootUrl = loc.rootUrl;
  const folders = path.slice(0, -1);
  const parentUrl = folders.length ? `/files/${folders[folders.length - 1].id}` : rootUrl;

  const toolbar = (
    <>
      <ToolButton
        icon={FolderOpenIcon}
        label={t("Open file location")}
        showLabel
        className="h-9 px-2.5 text-[13px]"
        disabled={!node}
        onClick={() => navigate(parentUrl)}
      />
      <ToolSeparator />
      <ToolButton
        icon={DownloadIcon}
        label={t("Download")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!node}
        onClick={() => node && triggerDownload(privateSource.contentUrl(node, true))}
      />
      <ToolButton
        icon={Share2Icon}
        label={t("Share")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!node || !caps.share}
        onClick={() => setDialog("share")}
      />
      <ToolButton
        icon={PencilIcon}
        label={t("Rename")}
        className="size-9 px-0 [&_svg]:size-[18px]"
        disabled={!node || !caps.write}
        onClick={() => setDialog("rename")}
      />
      {canEditSheet && (
        <>
          {/* Excel-specific features are kept separate from general file actions */}
          <ToolSeparator />
          <Button
            variant={sheetEditing ? "secondary" : "ghost"}
            className="h-9 gap-1.5 px-2.5 text-[13px] text-emerald-700 dark:text-emerald-400 [&_svg]:size-[18px]"
            aria-pressed={sheetEditing}
            disabled={sheetEditing}
            onClick={() => node && setEditingId(node.id)}
          >
            <SheetIcon />
            {sheetEditing ? t("Editing workbook") : t("Edit workbook")}
          </Button>
        </>
      )}
      <span className="flex-1" />
      <Button
        variant={detailsOpen ? "secondary" : "ghost"}
        className="h-9 gap-1.5 px-2.5 text-[13px] [&_svg]:size-[18px]"
        aria-pressed={detailsOpen}
        onClick={() => setDetailsOpen(!detailsOpen)}
      >
        <PanelRightIcon />
        <span className="max-lg:hidden">{t("Details")}</span>
      </Button>
    </>
  );

  return (
    <Frame
      toolbar={toolbar}
      icon={FileIcon}
      crumbs={node ? loc.crumbs : [{ label: "…" }]}
      path={crumbPath(loc.crumbs)}
      upTo={node ? parentUrl : null}
      activeFolder={folders.length ? folders[folders.length - 1].id : undefined}
      searchPlaceholder={t("Search files")}
      footer={
        node && (
          <span>
            {typeLabel(node)} · {formatBytes(node.size)} · {t("Modified {date}", { date: formatWinDate(node.updated_at) })}
          </span>
        )
      }
    >
      <div className="relative flex min-h-0 flex-1">
        <ContextMenu disabled={!customMenu}>
          <ContextMenuTrigger className="flex min-w-0 flex-1 items-center justify-center overflow-auto bg-muted/40">
            {info.isLoading ? (
              <Loader2Icon className="size-6 animate-spin text-muted-foreground" />
            ) : info.error || !node ? (
              <div className="text-sm text-destructive">{info.error?.message ?? t("File not found")}</div>
            ) : info.data?.offline && node.kind === "file" ? (
              <OfflinePanel reason={info.data.offline} retrying={info.isFetching} onRetry={() => info.refetch()} />
            ) : (
              <div className="flex size-full items-center justify-center p-0 [&>img]:p-4 [&>video]:p-4">
                {sheetEditing ? (
                  <Suspense fallback={<Loader2Icon className="size-6 animate-spin text-muted-foreground" />}>
                    <SheetEditor
                      node={node}
                      source={privateSource}
                      onExit={() => setEditingId(null)}
                      onSaved={(n) => {
                        qc.setQueryData(["node", id], (old: typeof info.data) => (old ? { ...old, node: { ...old.node, ...n } } : old));
                        qc.invalidateQueries({ queryKey: ["children"] });
                        qc.invalidateQueries({ queryKey: ["recent"] });
                      }}
                    />
                  </Suspense>
                ) : (
                  <FileViewer
                    node={node}
                    source={privateSource}
                    editable={caps.write}
                    embedded
                    onSaved={(n) => {
                      qc.setQueryData(["node", id], (old: typeof info.data) => (old ? { ...old, node: { ...old.node, ...n } } : old));
                      qc.invalidateQueries({ queryKey: ["children"] });
                      qc.invalidateQueries({ queryKey: ["recent"] });
                    }}
                  />
                )}
              </div>
            )}
          </ContextMenuTrigger>
          <ContextMenuContent>
            <DropdownMenuItem onClick={() => node && triggerDownload(privateSource.contentUrl(node, true))}>
              <DownloadIcon /> {t("Download")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => navigate(parentUrl)}>
              <FolderOpenIcon /> {t("Open file location")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => setDialog("access")}>
              <UsersRoundIcon /> {t("Share with…")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!caps.share} onClick={() => setDialog("share")}>
              <Share2Icon /> {t("Create share link")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!caps.write} onClick={() => setDialog("rename")}>
              <PencilIcon /> {t("Rename")}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => setDetailsOpen(true)}>
              <InfoIcon /> {t("Properties")}
            </DropdownMenuItem>
          </ContextMenuContent>
        </ContextMenu>
        {detailsOpen && node && <DetailsPane selected={[node]} onClose={() => setDetailsOpen(false)} />}
      </div>
      {dialog === "rename" && node && (
        <NameDialog
          title={t("Rename")}
          initial={node.name}
          confirmText={t("Rename")}
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            await api.rename(node.id, name);
            setDialog(null);
            invalidateFiles(qc);
          }}
        />
      )}
      {dialog === "share" && node && <ShareDialog node={node} onClose={() => setDialog(null)} />}
      {dialog === "access" && node && <AccessDialog nodeId={node.id} onClose={() => setDialog(null)} />}
    </Frame>
  );
}
