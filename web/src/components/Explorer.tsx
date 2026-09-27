import { useMemo, useRef, type ReactNode } from "react";
import { FolderOpenIcon, Grid2X2Icon, ListIcon, UploadCloudIcon, type LucideIcon } from "lucide-react";
import { api, privateSource, type Node, type Role, type SortKey, type SortOrder } from "@/api";
import { Button } from "@/components/ui/button";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { DetailsPane } from "@/components/DetailsPane";
import { ErrorState } from "@/components/ErrorState";
import { FileList } from "@/components/FileList";
import { MarqueeBox, useMarquee, type MeasureHits } from "@/components/useMarquee";
import { Frame, type Crumb } from "@/components/Frame";
import { setClipboard } from "@/lib/clipboard";
import { t } from "@/lib/i18n";
import { toastWithUndo } from "@/lib/undo";
import { formatBytes } from "@/lib/utils";
import { enqueue, filesFromInput } from "@/uploads";
import { useExplorerState } from "./explorer/state";
import { useExplorerActions } from "./explorer/actions";
import { explorerMenus } from "./explorer/menus";
import { explorerToolbar } from "./explorer/toolbar";
import { ExplorerDialogs } from "./explorer/dialogs";
import { SelectionBar } from "./explorer/selectionBar";
import { useMediaQuery } from "@/lib/focus";
import type { Item } from "./explorer/types";

export interface ExplorerProps {
  /** Notice shown above the file list (e.g. storage service offline) */
  notice?: ReactNode;
  /** Why the storage service is offline: disables uploads (new folder and paste only touch the database, so they still work) */
  offline?: string | null;
  items: Item[];
  loading: boolean;
  /** Further pages of a large folder are still loading in the background */
  loadingMore?: boolean;
  error?: Error | null;
  /** Current folder; when set, uploading and creating are possible */
  folderId?: string;
  /** A folder space: nothing can be changed from the web yet */
  readOnly?: boolean;
  crumbs: Crumb[];
  path?: string;
  upTo?: string | null;
  showLocation?: boolean;
  /** Show the uploader column (shared directories) */
  showOwner?: boolean;
  emptyHint?: string;
  sort?: { key: SortKey; order: SortOrder };
  /** Column header clicked: toggle sorting */
  onSort?(key: SortKey): void;
  /** "Sort" menu: set the sort order directly */
  onSortChange?(sort: { key: SortKey; order: SortOrder }): void;
  /** Address bar icon */
  icon?: LucideIcon;
  /** Role in the current folder */
  role?: Role;
  /** Current folder node (shown in the details pane when nothing is selected) */
  folder?: Node;
  empty?: ReactNode;
}

export function Explorer(p: ExplorerProps) {
  const s = useExplorerState(p);
  const a = useExplorerActions(p, s);
  const { newItems, menuItems } = explorerMenus(p, s, a);
  const toolbar = explorerToolbar(p, s, a, newItems);
  const {
    caps,
    tabs,
    clip,
    fileInput,
    dirInput,
    canUpload,
    selectedNodes,
    view,
    setView,
    selected,
    setSelected,
    anchor,
    setAnchor,
    dragging,
    showCheckboxes,
    detailsOpen,
    setDetailsOpen,
    dialog,
    setDialog,
  } = s;
  const { open, dropInto, uploadInto, dragProps, refresh } = a;
  // Hold the left button and drag on empty space to marquee-select (disabled while renaming); the list gives its row geometry
  const measure = useRef<MeasureHits>(null);
  const marquee = useMarquee({ selected, onSelect: setSelected, enabled: !p.loading && dialog?.t !== "rename", measure });
  // Phones: a bar with the selected items' actions takes the place of the context menu on a long press
  const phone = useMediaQuery("(max-width: 47.99rem)");
  const dimmed = useMemo(() => (clip?.mode === "cut" ? new Set(clip.ids) : undefined), [clip]);
  const footer = (
    <>
      <span>
        {t("{n} item|{n} items", { n: p.items.length })}
        {p.loadingMore && ` · ${t("Loading more items…")}`}
      </span>
      {selectedNodes.length > 0 && (
        <span className="border-l pl-3">
          {t("{n} item selected|{n} items selected", { n: selectedNodes.length })}
          {selectedNodes.some((n) => n.kind === "file") && `  ${formatBytes(selectedNodes.reduce((s, n) => s + n.size, 0))}`}
        </span>
      )}
      {clip && (
        <span className="border-l pl-3">
          {clip.mode === "cut" ? t("Clipboard: {n} item cut|Clipboard: {n} items cut", { n: clip.ids.length }) : t("Clipboard: {n} item copied|Clipboard: {n} items copied", { n: clip.ids.length })}
          <button type="button" className="ml-1.5 underline hover:text-foreground" onClick={() => setClipboard(null)}>
            {t("Clear")}
          </button>
        </span>
      )}
    </>
  );

  const footerRight = (
    <span className="flex items-center gap-0.5">
      <Button variant={view === "list" ? "secondary" : "ghost"} aria-pressed={view === "list"} size="icon-xs" aria-label={t("Details")} title={t("Details")} onClick={() => setView("list")}>
        <ListIcon />
      </Button>
      <Button variant={view === "grid" ? "secondary" : "ghost"} aria-pressed={view === "grid"} size="icon-xs" aria-label={t("Large icons")} title={t("Large icons")} onClick={() => setView("grid")}>
        <Grid2X2Icon />
      </Button>
    </span>
  );

  return (
    <Frame
      toolbar={toolbar}
      crumbs={p.crumbs}
      icon={p.icon}
      path={p.path}
      upTo={p.upTo}
      activeFolder={p.folderId}
      footer={footer}
      footerRight={footerRight}
      keys
    >
      {p.notice}
      <div className="relative flex min-h-0 flex-1">
        <ContextMenu>
          <ContextMenuTrigger
            className="relative min-h-0 flex-1 overflow-auto outline-none"
            onContextMenuCapture={(e) => {
              if (!(e.target as HTMLElement).closest("[data-node-id]")) setSelected(new Set());
            }}
            onClick={(e) => {
              if (!(e.target as HTMLElement).closest("[data-node-id]")) setSelected(new Set());
            }}
            {...dragProps}
            {...marquee.containerProps}
          >
            <MarqueeBox store={marquee.box} />
            {p.loading ? (
              <div className="grid gap-1.5 p-3">
                {Array.from({ length: 8 }, (_, i) => (
                  <Skeleton key={i} className="h-6 w-full" />
                ))}
              </div>
            ) : p.error ? (
              <ErrorState message={p.error.message} onRetry={a.refresh} />
            ) : (
              <FileList
                items={p.items}
                view={view}
                source={privateSource}
                selected={selected}
                anchor={anchor}
                onSelect={(s, a) => {
                  setSelected(s);
                  if (a !== undefined) setAnchor(a);
                }}
                onOpen={open}
                onOpenInNewTab={(n) => tabs.open(n.kind === "folder" ? `/files/${n.id}` : `/view/${n.id}`, { reuse: n.kind === "file" })}
                sort={p.sort}
                onSort={p.onSort}
                showLocation={p.showLocation}
                showOwner={p.showOwner}
                showCheckboxes={showCheckboxes}
                touchMenu={!phone}
                dimmed={dimmed}
                measureRef={measure}
                navRef={s.listNav}
                onDropInto={caps.write && p.folderId ? dropInto : undefined}
                onUploadInto={s.canUpload ? uploadInto : undefined}
                renamingId={dialog?.t === "rename" ? dialog.node.id : null}
                onRename={async (n, name) => {
                  await api.rename(n.id, name);
                  refresh();
                  if (name !== n.name)
                    toastWithUndo(t("Renamed to \"{name}\"", { name }), { undo: () => api.rename(n.id, n.name), undoneText: t("Renamed back"), after: refresh });
                }}
                onRenameDone={() => setDialog(null)}
                empty={
                  p.empty ?? (
                    <div className="flex min-h-52 flex-col items-center justify-center gap-2 py-10 text-muted-foreground">
                      <FolderOpenIcon className="size-9 stroke-[1.4]" />
                      <p>{t("No files here yet")}</p>
                      {canUpload && <p className="text-xs">{t("Drag files or folders here to upload them.")}</p>}
                      {p.offline && <p className="text-xs">{t("Storage service offline. You can't upload right now.")}</p>}
                      {p.emptyHint && <p className="text-xs">{p.emptyHint}</p>}
                    </div>
                  )
                }
              />
            )}
            {dragging && (
              <div className="pointer-events-none absolute inset-2 z-20 flex items-center justify-center rounded-lg border-2 border-dashed border-brand bg-brand/10">
                <div className="flex flex-col items-center gap-2 rounded-lg bg-background px-6 py-4 text-sm shadow-lg">
                  <UploadCloudIcon className="size-8 text-brand" />
                  {t("Drop to upload to this folder")}
                </div>
              </div>
            )}
          </ContextMenuTrigger>
          <ContextMenuContent>{menuItems}</ContextMenuContent>
        </ContextMenu>
        {detailsOpen && <DetailsPane selected={selectedNodes} folder={p.folder} onClose={() => setDetailsOpen(false)} />}
      </div>
      {phone && selectedNodes.length > 0 && <SelectionBar p={p} s={s} a={a} menuItems={menuItems} />}

      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          if (e.target.files?.length) enqueue(filesFromInput(e.target.files), p.folderId!);
          e.target.value = "";
        }}
      />
      <input
        ref={dirInput}
        type="file"
        hidden
        // @ts-expect-error webkitdirectory isn't in the standard types
        webkitdirectory=""
        onChange={(e) => {
          if (e.target.files?.length) enqueue(filesFromInput(e.target.files), p.folderId!);
          e.target.value = "";
        }}
      />

      <ExplorerDialogs p={p} s={s} a={a} />
    </Frame>
  );
}
