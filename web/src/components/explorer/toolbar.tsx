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
import type { SortKey, SortOrder } from "@/api";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ColumnChoices, listColumns } from "@/components/fileList/columns";
import type { ViewMode } from "@/components/fileList/layout";
import { ToolButton, ToolSeparator } from "@/components/Frame";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { shortcut } from "@/lib/keys";
import { cn } from "@/lib/utils";
import type { ExplorerProps } from "../Explorer";
import type { ExplorerState } from "./state";
import type { ExplorerActions } from "./actions";
import { Kbd } from "./ui";
import { t } from "@/lib/i18n";
import type { GroupBy } from "@/lib/listView";

const SORTS: [SortKey, string][] = [
  ["name", t("Name")],
  ["updated", t("Date modified")],
  ["created", t("Date created")],
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
  const { caps, canCreate, single, allFavorite, view, setView, groupBy, setGroupBy, setSelected, setDialog, showCheckboxes, setShowCheckboxes, detailsOpen, setDetailsOpen } = s;
  const { download, toggleFavorite, cut, copy, canPaste, paste } = a;
  const none = s.count === 0;
  // Windows 11 style command bar: New | Cut Copy Paste Rename Share Delete | Sort | View | ⋯ | Details
  const icon = "size-9 px-0 [&_svg]:size-[18px]";
  const phoneHidden = "max-md:hidden";
  const toolbar = (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={PlusCircleIcon} label={t("New")} showLabel phoneLabel disabled={!canCreate} className="h-9 px-2.5 text-[13px]" />} />
        <DropdownMenuContent className="w-56">{newItems}</DropdownMenuContent>
      </DropdownMenu>
      {/* Phones: what works on the selected items is in the bar that shows below the list while items are selected (and
          in its menu), so the toolbar keeps to one row; Paste stays while there is something to paste */}
      <ToolSeparator className="max-md:hidden" />
      <ToolButton icon={ScissorsIcon} label={t("Cut")} title={`${t("Cut")} (${shortcut("Ctrl+X")})`} className={cn(icon, phoneHidden)} disabled={none || !caps.write} onClick={cut} />
      <ToolButton icon={CopyIcon} label={t("Copy")} title={`${t("Copy")} (${shortcut("Ctrl+C")})`} className={cn(icon, phoneHidden)} disabled={none} onClick={copy} />
      <ToolButton
        icon={ClipboardPasteIcon}
        label={t("Paste")}
        title={`${t("Paste")} (${shortcut("Ctrl+V")})`}
        className={cn(icon, !canPaste && phoneHidden)}
        disabled={!canPaste}
        onClick={paste}
      />
      <ToolButton
        icon={PencilIcon}
        label={t("Rename")}
        title={`${t("Rename")} (F2)`}
        className={cn(icon, phoneHidden)}
        disabled={!single || !caps.write}
        onClick={() => single && setDialog({ t: "rename", node: single })}
      />
      <ToolButton
        icon={UsersRoundIcon}
        label={t("Share with…")}
        className={cn(icon, phoneHidden)}
        disabled={!single}
        onClick={() => single && setDialog({ t: "access", nodeId: single.id })}
      />
      <ToolButton
        icon={Share2Icon}
        label={t("Create share link")}
        className={cn(icon, phoneHidden)}
        disabled={!single || !caps.share}
        onClick={() => single && setDialog({ t: "share", node: single })}
      />
      <ToolButton
        icon={Trash2Icon}
        label={t("Delete")}
        title={`${t("Delete")} (Delete)`}
        className={cn(icon, phoneHidden)}
        disabled={none || !caps.del}
        onClick={() => setDialog({ t: "trash", picked: s.picked })}
      />
      <ToolSeparator className="max-md:hidden" />
      {p.sort && p.onSortChange && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<ToolButton icon={ArrowDownUpIcon} label={t("Sort")} showLabel phoneLabel className="h-9 px-2.5 text-[13px]" />} />
          <DropdownMenuContent className="w-44">
            {/* Radio items, so screen readers say which one is chosen */}
            <DropdownMenuRadioGroup value={p.sort.key} onValueChange={(k) => p.onSortChange!({ key: k as SortKey, order: p.sort!.order })}>
              {SORTS.map(([k, label]) => (
                <DropdownMenuRadioItem key={k} value={k} closeOnClick>
                  {label}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
            <DropdownMenuRadioGroup value={p.sort.order} onValueChange={(o) => p.onSortChange!({ key: p.sort!.key, order: o as SortOrder })}>
              <DropdownMenuRadioItem value="asc" closeOnClick>
                {t("Ascending")}
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="desc" closeOnClick>
                {t("Descending")}
              </DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={LayoutListIcon} label={t("View")} showLabel phoneLabel className="h-9 px-2.5 text-[13px]" />} />
        <DropdownMenuContent className="w-52">
          <DropdownMenuRadioGroup value={view} onValueChange={(v) => setView(v as ViewMode)}>
            {VIEWS.map(([v, Icon, label]) => (
              <DropdownMenuRadioItem key={v} value={v} closeOnClick>
                <Icon /> {label}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <GroupIcon /> {t("Group by")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-44">
              <DropdownMenuRadioGroup value={groupBy} onValueChange={(g) => setGroupBy(g as GroupBy)}>
                {GROUPS.map(([g, label]) => (
                  <DropdownMenuRadioItem key={g} value={g} closeOnClick>
                    {label}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={view !== "list"}>
              <Columns3Icon /> {t("Columns")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-52">
              <ColumnChoices columns={listColumns(p)} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          <DropdownMenuCheckboxItem checked={detailsOpen} onCheckedChange={(on) => setDetailsOpen(on)} closeOnClick>
            <PanelRightIcon /> {t("Details pane")}
          </DropdownMenuCheckboxItem>
          <DropdownMenuCheckboxItem checked={showCheckboxes} onCheckedChange={(on) => setShowCheckboxes(on)} closeOnClick>
            <SquareCheckIcon /> {t("Item check boxes")}
          </DropdownMenuCheckboxItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={EllipsisIcon} label={t("See more")} className={icon} />} />
        <DropdownMenuContent className="w-48">
          <DropdownMenuItem disabled={none} onClick={() => download(s.picked)}>
            <DownloadIcon /> {t("Download")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={toggleFavorite}>
            {allFavorite ? <StarOffIcon /> : <StarIcon />} {allFavorite ? t("Remove from favorites") : t("Add to favorites")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "move", picked: s.picked })}>
            <FolderInputIcon /> {t("Move to…")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "copy", picked: s.picked })}>
            <CopyIcon /> {t("Copy to…")}
          </DropdownMenuItem>
          {p.folderId && (
            <DropdownMenuItem onClick={() => setDialog({ t: "access", nodeId: p.folderId! })}>
              <UsersRoundIcon /> {p.folder?.parent_id ? t("Access to this folder") : t("Space members")}
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={s.selectAll}>
            <SquareCheckIcon /> {t("Select all")} <Kbd>Ctrl+A</Kbd>
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={() => setSelected(new Set())}>
            <XSquareIcon /> {t("Select none")} <Kbd>Esc</Kbd>
          </DropdownMenuItem>
          {/* With nothing selected, inverting selects everything (like File Explorer) */}
          <DropdownMenuItem disabled={!s.canInvert} onClick={s.invert}>
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
        // The label is hidden on narrow screens: the button still needs a name
        aria-label={t("Details pane")}
        title={`${t("Details pane")} (${shortcut("Alt+Enter")})`}
        onClick={() => setDetailsOpen(!detailsOpen)}
      >
        <PanelRightIcon />
        <span className="max-lg:hidden">{t("Details pane")}</span>
      </Button>
    </>
  );

  return toolbar;
}
