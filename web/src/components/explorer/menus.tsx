/**
 * Context menu: item actions when items are selected; new, paste etc. when clicking empty space. The Windows style
 * arranges them as Windows 11 does (lib/windowsBehaviour `menus`): submenus on empty space, a row of icon buttons on items.
 */
import { Fragment, type KeyboardEvent, type ReactNode } from "react";
import {
  ArrowDownUpIcon,
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
  GroupIcon,
  InfoIcon,
  LayoutListIcon,
  PanelTopIcon,
  PencilIcon,
  PlusCircleIcon,
  RefreshCwIcon,
  ScissorsIcon,
  Share2Icon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
  Undo2Icon,
  UploadIcon,
  UsersRoundIcon,
  CloudOffIcon,
  type LucideIcon,
} from "lucide-react";
import { DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger } from "@/components/ui/dropdown-menu";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";
import { Kbd } from "./ui";
import { GroupChoices, SortChoices, ViewChoices } from "./viewChoices";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { undoLast } from "@/lib/undo";
import { isZip } from "@/components/FileIcon";

/** The parts of a menu that have something in them, with a line between each */
function sections(...parts: ReactNode[][]) {
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

/**
 * The icon buttons at the top of an item's menu (Windows 11): Left and Right go along them, Down to the items below
 * and Up round to the last item
 */
function IconRow({ children }: { children: ReactNode }) {
  return (
    <DropdownMenuGroup aria-label={t("Common actions")} className="mb-1 flex items-center justify-between gap-0.5 border-b px-0.5 pb-1">
      {children}
    </DropdownMenuGroup>
  );
}

function IconItem({ icon: Icon, label, keys, destructive, ...props }: { icon: LucideIcon; label: string; keys?: string; destructive?: boolean; disabled?: boolean; onClick(): void }) {
  const moveAlong = (e: KeyboardEvent<HTMLElement>) => {
    const items = (el: Element | null | undefined) => [...(el?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
    const row = items(e.currentTarget.parentElement);
    const all = items(e.currentTarget.closest("[role=menu]"));
    let next: HTMLElement | undefined;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      const at = row.indexOf(e.currentTarget) + (e.key === "ArrowRight" ? 1 : -1);
      next = row[(at + row.length) % row.length];
    } else if (e.key === "ArrowDown") next = all[all.indexOf(row[row.length - 1]) + 1] ?? all[0];
    else if (e.key === "ArrowUp") next = all[all.length - 1];
    if (!next) return;
    e.preventDefault();
    e.stopPropagation();
    next.focus();
  };
  return (
    <DropdownMenuItem
      {...props}
      aria-label={label}
      title={keys ? `${label} (${shortcut(keys)})` : label}
      variant={destructive ? "destructive" : "default"}
      className="size-9 justify-center px-0 py-0 [&_svg:not([class*='size-'])]:size-[18px]"
      onKeyDown={moveAlong}
    >
      <Icon />
    </DropdownMenuItem>
  );
}

export function explorerMenus(p: ExplorerProps, s: ExplorerState, a: ExplorerActions) {
  const { caps, navigate, tabs, fileInput, dirInput, canCreate, canUpload, single, allFavorite, setDialog, setDetailsOpen } = s;
  const { refresh, open, download, compress, extract, toggleFavorite, cut, copy, canPaste, paste, createNew } = a;
  const offlineNote = canCreate && p.offline && (
    <p key="offline" className="flex items-start gap-1.5 px-2 pt-1 pb-1.5 text-[11px] leading-snug text-muted-foreground">
      <CloudOffIcon className="mt-px size-3.5 shrink-0" /> {t("Storage service offline. You can't upload or create files right now.")}
    </p>
  );
  // The command bar's New menu
  const newItems = (
    <>
      <DropdownMenuItem disabled={!canCreate} onClick={() => createNew("folder")}>
        <FolderPlusIcon /> {t("New folder")} <Kbd>Ctrl+Shift+N</Kbd>
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
      <InfoIcon /> {t("Properties")} <Kbd>Alt+Enter</Kbd>
    </DropdownMenuItem>
  );
  const rename = () => single && setDialog({ t: "rename", node: single });
  const remove = () => setDialog({ t: "trash", picked: s.picked });

  // Items for empty space
  const pasteItem = canCreate && (
    <DropdownMenuItem key="paste" disabled={!canPaste} onClick={paste}>
      <ClipboardPasteIcon /> {t("Paste")} <Kbd>Ctrl+V</Kbd>
    </DropdownMenuItem>
  );
  const refreshItem = (key: string) => (
    <DropdownMenuItem key={key} onClick={refresh}>
      <RefreshCwIcon /> {t("Refresh")} <Kbd>F5</Kbd>
    </DropdownMenuItem>
  );
  const properties = (
    <DropdownMenuItem key="properties" onClick={() => setDetailsOpen(true)}>
      <InfoIcon /> {t("Properties")}
    </DropdownMenuItem>
  );

  let menuItems: ReactNode;
  if (s.behaviour.menus && s.count) {
    // Windows 11: the common commands as icon buttons at the top, the others below
    menuItems = (
      <>
        <IconRow>
          <IconItem icon={ScissorsIcon} label={t("Cut")} keys="Ctrl+X" disabled={!caps.write} onClick={cut} />
          <IconItem icon={CopyIcon} label={t("Copy")} keys="Ctrl+C" onClick={copy} />
          <IconItem icon={PencilIcon} label={t("Rename")} keys="F2" disabled={!single || !caps.write} onClick={rename} />
          {/* A share link where the role allows one, else sharing with people */}
          <IconItem
            icon={Share2Icon}
            label={t("Share")}
            disabled={!single}
            onClick={() => single && setDialog(caps.share ? { t: "share", node: single } : { t: "access", nodeId: single.id })}
          />
          <IconItem icon={Trash2Icon} label={t("Delete")} keys="Delete" destructive disabled={!caps.del} onClick={remove} />
        </IconRow>
        {sections([openItem, openInNewTab, openLocation], [downloadItem, compressItem, extractItem], [favorite, shareLink, shareWith], [moveTo, copyTo], [itemProperties])}
      </>
    );
  } else if (s.behaviour.menus) {
    // Windows 11: how the list shows, then what can be done here; New and Upload are submenus
    menuItems = sections(
      [
        <DropdownMenuSub key="view">
          <DropdownMenuSubTrigger>
            <LayoutListIcon /> {t("View")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-48">
            <ViewChoices view={s.view} onChange={s.setView} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>,
        p.sort && p.onSortChange && (
          <DropdownMenuSub key="sort">
            <DropdownMenuSubTrigger>
              <ArrowDownUpIcon /> {t("Sort by")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-44">
              <SortChoices sort={p.sort} onChange={p.onSortChange} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        ),
        <DropdownMenuSub key="group">
          <DropdownMenuSubTrigger>
            <GroupIcon /> {t("Group by")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-44">
            <GroupChoices groupBy={s.groupBy} onChange={s.setGroupBy} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>,
      ],
      [
        // Named for what it takes back, and only when there is something
        s.undoLabel && (
          <DropdownMenuItem key="undo" onClick={() => undoLast()}>
            <Undo2Icon /> {s.undoLabel} <Kbd>Ctrl+Z</Kbd>
          </DropdownMenuItem>
        ),
        refreshItem("refresh"),
      ],
      [pasteItem],
      [
        canCreate && (
          <DropdownMenuSub key="new">
            <DropdownMenuSubTrigger>
              <PlusCircleIcon /> {t("New")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-56">
              <DropdownMenuItem onClick={() => createNew("folder")}>
                <FolderPlusIcon /> {t("Folder")} <Kbd>Ctrl+Shift+N</Kbd>
              </DropdownMenuItem>
              <DropdownMenuItem disabled={!canUpload} onClick={() => createNew("file")}>
                <FilePlusIcon /> {t("Text document")}
              </DropdownMenuItem>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        ),
        // Not in File Explorer, but a web app needs it: a submenu, out of the way
        canCreate && (
          <DropdownMenuSub key="upload">
            <DropdownMenuSubTrigger disabled={!canUpload}>
              <UploadIcon /> {t("Upload")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-44">
              <DropdownMenuItem onClick={() => fileInput.current?.click()}>
                <UploadIcon /> {t("Files")}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => dirInput.current?.click()}>
                <FolderUpIcon /> {t("Folder")}
              </DropdownMenuItem>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
        ),
        offlineNote,
      ],
      [properties],
    );
  } else if (s.count) {
    menuItems = (
      <>
        {openItem}
        {openInNewTab}
        {openLocation}
        {downloadItem}
        {compressItem}
        {extractItem}
        {shareWith}
        {shareLink}
        {favorite}
        <DropdownMenuSeparator />
        {caps.write && (
          <DropdownMenuItem onClick={cut}>
            <ScissorsIcon /> {t("Cut")} <Kbd>Ctrl+X</Kbd>
          </DropdownMenuItem>
        )}
        <DropdownMenuItem onClick={copy}>
          <CopyIcon /> {t("Copy")} <Kbd>Ctrl+C</Kbd>
        </DropdownMenuItem>
        {caps.write && single && (
          <DropdownMenuItem onClick={rename}>
            <PencilIcon /> {t("Rename")} <Kbd>F2</Kbd>
          </DropdownMenuItem>
        )}
        {moveTo}
        {copyTo}
        {caps.del && (
          <DropdownMenuItem variant="destructive" onClick={remove}>
            <Trash2Icon /> {t("Delete")} <Kbd>Delete</Kbd>
          </DropdownMenuItem>
        )}
        <DropdownMenuSeparator />
        {itemProperties}
      </>
    );
  } else {
    menuItems = (
      <>
        {canCreate && newItems}
        {canCreate && <DropdownMenuSeparator />}
        {pasteItem}
        {refreshItem("refresh")}
        {properties}
      </>
    );
  }

  return { newItems, menuItems };
}
