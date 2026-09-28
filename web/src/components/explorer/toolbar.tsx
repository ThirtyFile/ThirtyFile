/** Windows 11 style command bar: New | Cut Copy Paste Rename Share Delete | Sort | View | ⋯ | Details */
import {
  ArrowDownUpIcon,
  ClipboardPasteIcon,
  CopyIcon,
  DownloadIcon,
  EllipsisIcon,
  FolderInputIcon,
  Columns3Icon,
  Grid2X2Icon,
  Grid3X3Icon,
  GroupIcon,
  KeyboardIcon,
  LayoutGridIcon,
  LayoutListIcon,
  ListIcon,
  PanelRightIcon,
  PencilIcon,
  PlusCircleIcon,
  ScissorsIcon,
  Share2Icon,
  SquareCheckIcon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
  UsersRoundIcon,
  XSquareIcon,
  type LucideIcon,
} from "lucide-react";
import type { SortKey } from "@/api";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ColumnChoices, listColumns, type ViewMode } from "@/components/FileList";
import { ToolButton, ToolSeparator } from "@/components/Frame";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { shortcut } from "@/lib/keys";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";
import { Check, Kbd } from "./ui";
import { t } from "@/lib/i18n";
import type { GroupBy } from "@/lib/listView";

const SORTS: [SortKey, string][] = [
  ["name", t("Name")],
  ["updated", t("Date modified")],
  ["type", t("Type")],
  ["size", t("Size")],
];

const VIEWS: [ViewMode, LucideIcon, string][] = [
  ["grid", Grid2X2Icon, t("Large icons")],
  ["medium", Grid3X3Icon, t("Medium icons")],
  ["compact", LayoutListIcon, t("List")],
  ["list", ListIcon, t("Details")],
  ["tiles", LayoutGridIcon, t("Tiles")],
];

const GROUPS: [GroupBy, string][] = [
  ["none", t("(None)")],
  ["type", t("Type")],
  ["date", t("Date modified")],
];

export function explorerToolbar(p: ExplorerProps, s: ExplorerState, a: ExplorerActions, newItems: React.ReactNode) {
  const {
    caps,
    canCreate,
    selectedNodes,
    selectedIds,
    single,
    allFavorite,
    view,
    setView,
    groupBy,
    setGroupBy,
    selected,
    setSelected,
    setDialog,
    showCheckboxes,
    setShowCheckboxes,
    detailsOpen,
    setDetailsOpen,
  } = s;
  const { download, toggleFavorite, cut, copy, canPaste, paste } = a;
  const none = selectedNodes.length === 0;
  // Windows 11 style command bar: New | Cut Copy Paste Rename Share Delete | Sort | View | ⋯ | Details
  const icon = "size-9 px-0 [&_svg]:size-[18px]";
  const toolbar = (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<ToolButton icon={PlusCircleIcon} label={t("New")} showLabel disabled={!canCreate} className="h-9 px-2.5 text-[13px]" />}
        />
        <DropdownMenuContent className="w-56">{newItems}</DropdownMenuContent>
      </DropdownMenu>
      <ToolSeparator />
      <ToolButton icon={ScissorsIcon} label={t("Cut")} title={`${t("Cut")} (${shortcut("Ctrl+X")})`} className={icon} disabled={none || !caps.write} onClick={cut} />
      <ToolButton icon={CopyIcon} label={t("Copy")} title={`${t("Copy")} (${shortcut("Ctrl+C")})`} className={icon} disabled={none} onClick={copy} />
      <ToolButton icon={ClipboardPasteIcon} label={t("Paste")} title={`${t("Paste")} (${shortcut("Ctrl+V")})`} className={icon} disabled={!canPaste} onClick={paste} />
      <ToolButton
        icon={PencilIcon}
        label={t("Rename")}
        title={`${t("Rename")} (F2)`}
        className={icon}
        disabled={!single || !caps.write}
        onClick={() => single && setDialog({ t: "rename", node: single })}
      />
      <ToolButton
        icon={UsersRoundIcon}
        label={t("Share with…")}
        className={icon}
        disabled={!single}
        onClick={() => single && setDialog({ t: "access", nodeId: single.id })}
      />
      <ToolButton
        icon={Share2Icon}
        label={t("Create share link")}
        className={icon}
        disabled={!single || !caps.share}
        onClick={() => single && setDialog({ t: "share", node: single })}
      />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete")}
        title={`${t("Delete")} (Delete)`}
        className={icon}
        disabled={none || !caps.del}
        onClick={() => setDialog({ t: "trash", ids: selectedIds })}
      />
      <ToolSeparator />
      {p.sort && p.onSortChange && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<ToolButton icon={ArrowDownUpIcon} label={t("Sort")} showLabel className="h-9 px-2.5 text-[13px]" />} />
          <DropdownMenuContent className="w-40">
            {SORTS.map(([k, label]) => (
              <DropdownMenuItem key={k} onClick={() => p.onSortChange!({ key: k, order: p.sort!.order })}>
                <Check on={p.sort!.key === k} /> {label}
              </DropdownMenuItem>
            ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => p.onSortChange!({ key: p.sort!.key, order: "asc" })}>
              <Check on={p.sort!.order === "asc"} /> {t("Ascending")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => p.onSortChange!({ key: p.sort!.key, order: "desc" })}>
              <Check on={p.sort!.order === "desc"} /> {t("Descending")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={LayoutListIcon} label={t("View")} showLabel className="h-9 px-2.5 text-[13px]" />} />
        <DropdownMenuContent className="w-48">
          {VIEWS.map(([v, Icon, label]) => (
            <DropdownMenuItem key={v} onClick={() => setView(v)}>
              <Check on={view === v} /> <Icon /> {label}
            </DropdownMenuItem>
          ))}
          <DropdownMenuSeparator />
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <Check on={groupBy !== "none"} /> <GroupIcon /> {t("Group by")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-44">
              {GROUPS.map(([g, label]) => (
                <DropdownMenuItem key={g} onClick={() => setGroupBy(g)}>
                  <Check on={groupBy === g} /> {label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={view !== "list"}>
              <Check on={false} /> <Columns3Icon /> {t("Columns")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-48">
              <ColumnChoices columns={listColumns(p)} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => setDetailsOpen(!detailsOpen)}>
            <Check on={detailsOpen} /> <PanelRightIcon /> {t("Details pane")}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => setShowCheckboxes(!showCheckboxes)}>
            <Check on={showCheckboxes} /> <SquareCheckIcon /> {t("Item check boxes")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={EllipsisIcon} label={t("See more")} className={icon} />} />
        <DropdownMenuContent className="w-48">
          <DropdownMenuItem disabled={none} onClick={() => download(selectedIds)}>
            <DownloadIcon /> {t("Download")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={toggleFavorite}>
            {allFavorite ? <StarOffIcon /> : <StarIcon />} {allFavorite ? t("Remove from favorites") : t("Add to favorites")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "move", ids: selectedIds })}>
            <FolderInputIcon /> {t("Move to…")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "copy", ids: selectedIds })}>
            <CopyIcon /> {t("Copy to…")}
          </DropdownMenuItem>
          {p.folderId && (
            <DropdownMenuItem onClick={() => setDialog({ t: "access", nodeId: p.folderId! })}>
              <UsersRoundIcon /> {p.folder?.parent_id ? t("Access to this folder") : t("Space members")}
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => setSelected(new Set(p.items.map((n) => n.id)))}>
            <SquareCheckIcon /> {t("Select all")} <Kbd>Ctrl+A</Kbd>
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={() => setSelected(new Set())}>
            <XSquareIcon /> {t("Select none")} <Kbd>Esc</Kbd>
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={() => setSelected(new Set(p.items.filter((n) => !selected.has(n.id)).map((n) => n.id)))}>
            <SquareCheckIcon /> {t("Invert selection")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={openShortcuts}>
            <KeyboardIcon /> {t("Keyboard shortcuts")} <Kbd>?</Kbd>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <span className="flex-1" />
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

  return toolbar;
}
