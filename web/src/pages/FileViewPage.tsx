import { Suspense, lazy, useEffect, useEffectEvent, useState } from "react";
import { Navigate, useNavigate, useParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
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
import { api, privateSource, triggerDownload, type Node } from "@/api";
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
import { toastWithUndo } from "@/lib/undo";
import { categoryOf, isTextLike, typeLabel } from "@/components/FileIcon";
import { capsOf } from "@/lib/drives";
import { locationOf, useSort } from "@/pages/FilesPage";
import { useAllPages } from "@/lib/pages";

const SheetEditor = lazy(() => import("@/components/sheet/SheetEditor"));

/** Where focus takes the arrow keys for itself: typing, a media player's seek bar, lists, menus and the workbook */
const OWN_ARROWS = ".cm-editor, video, audio, input, textarea, select, [contenteditable], [role=grid], [role=tree], [role=tablist], [role=menu], [role=listbox], [role=slider], [data-slot=dialog-content]";

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
  // Previous / next file of the folder, in the order the folder is sorted in (the list shares these pages)
  const [sort] = useSort();
  const parentId = node?.parent_id ?? undefined;
  const siblings = useAllPages(
    ["children", parentId, sort.key, sort.order],
    (limit, after) => api.childrenPage(parentId!, sort.key, sort.order, limit, after),
    !!parentId && node?.kind === "file",
  );
  const files = siblings.items.filter((n) => n.kind === "file");
  const at = node ? files.findIndex((n) => n.id === node.id) : -1;
  const prev = at > 0 ? files[at - 1] : undefined;
  const next = at >= 0 ? files[at + 1] : undefined;
  const goTo = (n: { id: string } | undefined) => n && navigate(`/view/${n.id}`);
  // Images, media and unpreviewable files use a custom context menu; text, Word and Excel keep the browser menu so text can be copied
  const customMenu = !!node && ["image", "video", "audio", "other", "archive"].includes(categoryOf(node)) && !isTextLike(node);
  const sheetEditingNow = !!node && editingId === node.id;
  // An effect event: the listener always sees the current neighbours without subscribing again
  const onArrowKey = useEffectEvent((e: KeyboardEvent) => {
    if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || dialog || sheetEditingNow) return;
    if ((e.target as HTMLElement)?.closest?.(OWN_ARROWS) || document.querySelector("[data-slot=dialog-content], [role=menu]")) return;
    const target = e.key === "ArrowLeft" ? prev : e.key === "ArrowRight" ? next : undefined;
    if (!target) return;
    e.preventDefault();
    goTo(target);
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => onArrowKey(e);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (node?.kind === "folder") return <Navigate to={`/files/${node.id}`} replace />;

  const path = info.data?.path ?? [];
  const loc = locationOf(info.data);
  const caps = capsOf(info.data?.role, me, info.data?.read_only);
  // A save from the editor: the new version shows here, in the folder's list and in Recent
  const onSaved = (n: Node) => {
    qc.setQueryData(["node", id], (old: typeof info.data) => (old ? { ...old, node: { ...old.node, ...n } } : old));
    qc.invalidateQueries({ queryKey: ["children"] });
    qc.invalidateQueries({ queryKey: ["recent"] });
  };
  const canEditSheet = !!node && extOf(node.name) === "xlsx" && caps.write && node.size <= me.max_edit_bytes;
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
        label={t("Create share link")}
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
      {at >= 0 && files.length > 1 && (
        <>
          <ToolButton icon={ChevronLeftIcon} label={t("Previous (←)")} className="size-9 px-0 [&_svg]:size-[18px]" disabled={!prev} onClick={() => goTo(prev)} />
          <ToolButton icon={ChevronRightIcon} label={t("Next (→)")} className="size-9 px-0 [&_svg]:size-[18px]" disabled={!next} onClick={() => goTo(next)} />
          <ToolSeparator />
        </>
      )}
      <Button
        variant={detailsOpen ? "secondary" : "ghost"}
        className="h-9 gap-1.5 px-2.5 text-[13px] [&_svg]:size-[18px]"
        aria-pressed={detailsOpen}
        onClick={() => setDetailsOpen(!detailsOpen)}
      >
        <PanelRightIcon />
        <span className="max-lg:hidden">{t("Details pane")}</span>
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
            {at >= 0 && files.length > 1 && ` · ${t("{n} of {total}", { n: at + 1, total: files.length })}`}
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
                      onSaved={onSaved}
                    />
                  </Suspense>
                ) : (
                  <FileViewer
                    node={node}
                    source={privateSource}
                    editable={caps.write}
                    embedded
                    onSaved={onSaved}
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
            const before = node.name;
            await api.rename(node.id, name);
            setDialog(null);
            invalidateFiles(qc);
            if (name !== before)
              toastWithUndo(t("Renamed to \"{name}\"", { name }), { undo: () => api.rename(node.id, before), undoneText: t("Renamed back"), after: () => invalidateFiles(qc) });
          }}
        />
      )}
      {dialog === "share" && node && <ShareDialog node={node} onClose={() => setDialog(null)} />}
      {dialog === "access" && node && <AccessDialog nodeId={node.id} onClose={() => setDialog(null)} />}
    </Frame>
  );
}
