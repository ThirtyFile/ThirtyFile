/**
 * Context menu: item actions when items are selected; new, paste etc. when clicking empty space. The entries are made
 * here, the same in every style; the style arranges them (`menu` in components/style): the Windows style as Windows 11
 * does, with submenus on empty space and a row of icon buttons on items.
 */
import { Fragment, type ReactNode } from "react";
import {
  ClipboardPasteIcon,
  CopyIcon,
  DownloadIcon,
  FileArchiveIcon,
  PackageOpenIcon,
  EyeIcon,
  FilePlusIcon,
  FolderInputIcon,
  FolderOpenIcon,
  FolderPlusIcon,
  FolderUpIcon,
  InfoIcon,
  PanelTopIcon,
  PencilIcon,
  RefreshCwIcon,
  ScissorsIcon,
  Share2Icon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
  UploadIcon,
  UsersRoundIcon,
  CloudOffIcon,
} from "lucide-react";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";
import { Kbd } from "./ui";
import { TagSubmenu } from "@/components/tags";
import { t } from "@/lib/i18n";
import { isZip } from "@/components/FileIcon";

/** The parts of a menu that have something in them, with a line between each */
export function sections(...parts: ReactNode[][]) {
  return parts
    .map((part) => part.filter(Boolean))
    .filter((part) => part.length)
    .map((part, i) => (
      <Fragment key={i}>
        {i > 0 && <DropdownMenuSeparator />}
        {part}
      </Fragment>
    ));
}

/** The entries of the explorer's menus, for the style to arrange (an entry that doesn't apply here is `false` or null) */
export interface MenuEntries {
  p: ExplorerProps;
  s: ExplorerState;
  a: ExplorerActions;
  /** What the command bar's New menu holds */
  newItems: ReactNode;
  /** Said under New and Upload while the storage service is offline */
  offlineNote: ReactNode;
  // On items
  openItem: ReactNode;
  openInNewTab: ReactNode;
  openLocation: ReactNode;
  downloadItem: ReactNode;
  compressItem: ReactNode;
  extractItem: ReactNode;
  shareWith: ReactNode;
  shareLink: ReactNode;
  favorite: ReactNode;
  /** "Tags ›": the person's own tags, put on the items or taken off */
  tags: ReactNode;
  moveTo: ReactNode;
  copyTo: ReactNode;
  itemProperties: ReactNode;
  rename(): void;
  remove(): void;
  // On empty space
  pasteItem: ReactNode;
  refreshItem(key: string): ReactNode;
  properties: ReactNode;
}

export function explorerMenus(p: ExplorerProps, s: ExplorerState, a: ExplorerActions) {
  const { caps, navigate, tabs, fileInput, dirInput, canCreate, canUpload, single, allFavorite, setDialog, setDetailsOpen } = s;
  const { refresh, open, download, compress, extract, toggleFavorite, canPaste, paste, createNew } = a;
  const k = s.kit.keys;
  const offlineNote = canCreate && p.offline && (
    <p key="offline" className="flex items-start gap-1.5 px-2 pt-1 pb-1.5 text-[11px] leading-snug text-muted-foreground">
      <CloudOffIcon className="mt-px size-3.5 shrink-0" /> {t("Storage service offline. You can't upload or create files right now.")}
    </p>
  );
  // The command bar's New menu
  const newItems = (
    <>
      <DropdownMenuItem disabled={!canCreate} onClick={() => createNew("folder")}>
        <FolderPlusIcon /> {t("New folder")} <Kbd>{k.newFolder[0]}</Kbd>
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!canUpload} onClick={() => createNew("file")}>
        <FilePlusIcon /> {t("New text document")}
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      <DropdownMenuItem disabled={!canUpload} onClick={() => fileInput.current?.click()}>
        <UploadIcon /> {t("Upload files")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!canUpload} onClick={() => dirInput.current?.click()}>
        <FolderUpIcon /> {t("Upload folder")}
      </DropdownMenuItem>
      {offlineNote}
    </>
  );

  // Items that work on what is selected
  const openItem = single && (
    <DropdownMenuItem key="open" onClick={() => open(single)}>
      {single.kind === "folder" ? <FolderOpenIcon /> : <EyeIcon />} {t("Open")}
    </DropdownMenuItem>
  );
  const openInNewTab = single?.kind === "folder" && (
    <DropdownMenuItem key="tab" onClick={() => tabs.open(`/files/${single.id}`)}>
      <PanelTopIcon /> {t("Open in new tab")}
    </DropdownMenuItem>
  );
  const openLocation = single?.location !== undefined && (
    <DropdownMenuItem key="location" onClick={() => navigate(`/files/${single.parent_id}`)}>
      <FolderOpenIcon /> {t("Open file location")}
    </DropdownMenuItem>
  );
  const downloadItem = (
    <DropdownMenuItem key="download" onClick={() => download(s.picked)}>
      <DownloadIcon /> {s.count > 1 || single?.kind === "folder" ? t("Download (ZIP)") : t("Download")}
    </DropdownMenuItem>
  );
  // Made next to the items, so only where new files can be added (not in search results or other lists)
  const compressItem = canUpload && (
    <DropdownMenuItem key="compress" onClick={() => compress(s.picked)}>
      <FileArchiveIcon /> {t("Compress to ZIP file")}
    </DropdownMenuItem>
  );
  const extractItem = canUpload && single && isZip(single) && (
    <DropdownMenuItem key="extract" onClick={() => extract(single)}>
      <PackageOpenIcon /> {t("Extract all")}
    </DropdownMenuItem>
  );
  const shareWith = single && (
    <DropdownMenuItem key="access" onClick={() => setDialog({ t: "access", nodeId: single.id })}>
      <UsersRoundIcon /> {t("Share with…")}
    </DropdownMenuItem>
  );
  const shareLink = single && caps.share && (
    <DropdownMenuItem key="link" onClick={() => setDialog({ t: "share", node: single })}>
      <Share2Icon /> {t("Create share link")}
    </DropdownMenuItem>
  );
  const favorite = (
    <DropdownMenuItem key="favorite" onClick={toggleFavorite}>
      {allFavorite ? <StarOffIcon /> : <StarIcon />} {allFavorite ? t("Remove from favorites") : t("Add to favorites")}
    </DropdownMenuItem>
  );
  const tags = <TagSubmenu key="tags" nodes={s.selectedNodes} picked={s.picked} />;
  const moveTo = caps.write && (
    <DropdownMenuItem key="move" onClick={() => setDialog({ t: "move", picked: s.picked })}>
      <FolderInputIcon /> {t("Move to…")}
    </DropdownMenuItem>
  );
  const copyTo = caps.write && (
    <DropdownMenuItem key="copy" onClick={() => setDialog({ t: "copy", picked: s.picked })}>
      <CopyIcon /> {t("Copy to…")}
    </DropdownMenuItem>
  );
  const itemProperties = (
    <DropdownMenuItem key="properties" onClick={() => setDetailsOpen(true)}>
      <InfoIcon /> {t("Properties")} <Kbd>{k.details[0]}</Kbd>
    </DropdownMenuItem>
  );
  const rename = () => single && setDialog({ t: "rename", node: single });
  const remove = () => setDialog({ t: "trash", picked: s.picked });

  // Items for empty space
  const pasteItem = canCreate && (
    <DropdownMenuItem key="paste" disabled={!canPaste} onClick={paste}>
      <ClipboardPasteIcon /> {t("Paste")} <Kbd>{k.paste[0]}</Kbd>
    </DropdownMenuItem>
  );
  const refreshItem = (key: string) => (
    <DropdownMenuItem key={key} onClick={refresh}>
      <RefreshCwIcon /> {t("Refresh")} <Kbd>{k.refresh[0]}</Kbd>
    </DropdownMenuItem>
  );
  const properties = (
    <DropdownMenuItem key="properties" onClick={() => setDetailsOpen(true)}>
      <InfoIcon /> {t("Properties")}
    </DropdownMenuItem>
  );

  const menuItems = s.kit.menu({
    p,
    s,
    a,
    newItems,
    offlineNote,
    openItem,
    openInNewTab,
    openLocation,
    downloadItem,
    compressItem,
    extractItem,
    shareWith,
    shareLink,
    favorite,
    tags,
    moveTo,
    copyTo,
    itemProperties,
    rename,
    remove,
    pasteItem,
    refreshItem,
    properties,
  });
  return { newItems, menuItems };
}

/**
 * The entries as plain lists, for a style that doesn't arrange them its own way: on items, open, share, favourite and
 * tags, then the clipboard, rename, move, copy and delete, then properties; on empty space, New and Upload, paste, refresh
 * and properties
 */
export function plainMenu(m: MenuEntries): ReactNode {
  const { s, a } = m;
  const { caps, single, canCreate } = s;
  const k = s.kit.keys;
  if (!s.count)
    return (
      <>
        {canCreate && m.newItems}
        {canCreate && <DropdownMenuSeparator />}
        {m.pasteItem}
        {m.refreshItem("refresh")}
        {m.properties}
      </>
    );
  return (
    <>
      {m.openItem}
      {m.openInNewTab}
      {m.openLocation}
      {m.downloadItem}
      {m.compressItem}
      {m.extractItem}
      {m.shareWith}
      {m.shareLink}
      {m.favorite}
      {m.tags}
      <DropdownMenuSeparator />
      {caps.write && (
        <DropdownMenuItem onClick={a.cut}>
          <ScissorsIcon /> {t("Cut")} <Kbd>{k.cut[0]}</Kbd>
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={a.copy}>
        <CopyIcon /> {t("Copy")} <Kbd>{k.copy[0]}</Kbd>
      </DropdownMenuItem>
      {caps.write && single && (
        <DropdownMenuItem onClick={m.rename}>
          <PencilIcon /> {t("Rename")} <Kbd>{k.rename[0]}</Kbd>
        </DropdownMenuItem>
      )}
      {m.moveTo}
      {m.copyTo}
      {caps.del && (
        <DropdownMenuItem variant="destructive" onClick={m.remove}>
          <Trash2Icon /> {t("Delete")} <Kbd>{k.trash[0]}</Kbd>
        </DropdownMenuItem>
      )}
      <DropdownMenuSeparator />
      {m.itemProperties}
    </>
  );
}
