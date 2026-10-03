/** The Windows style's context menu, arranged as Windows 11 does: a row of icon buttons on items, submenus on empty space */
import type { KeyboardEvent, ReactNode } from "react";
import {
  ArrowDownUpIcon,
  CopyIcon,
  FilePlusIcon,
  FolderPlusIcon,
  FolderUpIcon,
  GroupIcon,
  LayoutListIcon,
  PencilIcon,
  PlusCircleIcon,
  ScissorsIcon,
  Share2Icon,
  Trash2Icon,
  Undo2Icon,
  UploadIcon,
  type LucideIcon,
} from "lucide-react";
import { DropdownMenuGroup, DropdownMenuItem, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger } from "@/components/ui/dropdown-menu";
import { sections, type MenuEntries } from "@/components/explorer/menus";
import { Kbd } from "@/components/explorer/ui";
import { GroupChoices, SortChoices, ViewChoices } from "@/components/explorer/viewChoices";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { undoLast } from "@/lib/undo";

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

export function windowsMenu(m: MenuEntries): ReactNode {
  const { p, s, a } = m;
  const { caps, single, canCreate, canUpload, fileInput, dirInput, setDialog } = s;
  const k = s.kit.keys;
  if (s.count)
    // The common commands as icon buttons at the top, the others below
    return (
      <>
        <IconRow>
          <IconItem icon={ScissorsIcon} label={t("Cut")} keys={k.cut[0]} disabled={!caps.write} onClick={a.cut} />
          <IconItem icon={CopyIcon} label={t("Copy")} keys={k.copy[0]} onClick={a.copy} />
          <IconItem icon={PencilIcon} label={t("Rename")} keys={k.rename[0]} disabled={!single || !caps.write} onClick={m.rename} />
          {/* A share link where the role allows one, else sharing with people */}
          <IconItem
            icon={Share2Icon}
            label={t("Share")}
            disabled={!single}
            onClick={() => single && setDialog(caps.share ? { t: "share", node: single } : { t: "access", nodeId: single.id })}
          />
          <IconItem icon={Trash2Icon} label={t("Delete")} keys={k.trash[0]} destructive disabled={!caps.del} onClick={m.remove} />
        </IconRow>
        {sections(
          [m.openItem, m.openInNewTab, m.openLocation],
          [m.downloadItem, m.compressItem, m.extractItem],
          [m.favorite, m.tags, m.shareLink, m.shareWith],
          [m.moveTo, m.copyTo],
          [m.itemProperties],
        )}
      </>
    );
  // How the list shows, then what can be done here; New and Upload are submenus
  return sections(
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
        <DropdownMenuSubTrigger disabled={s.view === "columns"}>
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
          <Undo2Icon /> {s.undoLabel} <Kbd>{k.undo[0]}</Kbd>
        </DropdownMenuItem>
      ),
      m.refreshItem("refresh"),
    ],
    [m.pasteItem],
    [
      canCreate && (
        <DropdownMenuSub key="new">
          <DropdownMenuSubTrigger>
            <PlusCircleIcon /> {t("New")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-56">
            <DropdownMenuItem onClick={() => a.createNew("folder")}>
              <FolderPlusIcon /> {t("Folder")} <Kbd>{k.newFolder[0]}</Kbd>
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!canUpload} onClick={() => a.createNew("file")}>
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
      m.offlineNote,
    ],
    [m.properties],
  );
}
