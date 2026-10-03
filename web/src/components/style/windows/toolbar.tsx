/**
 * The Windows style's command bar, as in Windows 11: New | Cut Copy Paste Rename Share Delete | Sort | View | ⋯ |
 * Details
 */
import {
  ArrowDownUpIcon,
  ClipboardPasteIcon,
  CopyIcon,
  DownloadIcon,
  EllipsisIcon,
  FolderInputIcon,
  Columns3Icon,
  GroupIcon,
  KeyboardIcon,
  LayoutListIcon,
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
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ColumnChoices, listColumns } from "@/components/fileList/columns";
import { ToolButton, ToolSeparator } from "@/components/frame/ToolButton";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { shortcut } from "@/lib/keys";
import { cn } from "@/lib/utils";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerState } from "@/components/explorer/state";
import type { ExplorerActions } from "@/components/explorer/actions";
import { Kbd } from "@/components/explorer/ui";
import { GroupChoices, SortChoices, ViewChoices } from "@/components/explorer/viewChoices";
import { t } from "@/lib/i18n";

export function windowsToolbar(p: ExplorerProps, s: ExplorerState, a: ExplorerActions, newItems: React.ReactNode) {
  const { caps, canCreate, single, allFavorite, view, setView, groupBy, setGroupBy, setSelected, setDialog, showCheckboxes, setShowCheckboxes, detailsOpen, setDetailsOpen } = s;
  const { download, toggleFavorite, cut, copy, canPaste, paste } = a;
  const none = s.count === 0;
  const k = s.kit.keys;
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
      <ToolButton icon={ScissorsIcon} label={t("Cut")} title={`${t("Cut")} (${shortcut(k.cut[0])})`} className={cn(icon, phoneHidden)} disabled={none || !caps.write} onClick={cut} />
      <ToolButton icon={CopyIcon} label={t("Copy")} title={`${t("Copy")} (${shortcut(k.copy[0])})`} className={cn(icon, phoneHidden)} disabled={none} onClick={copy} />
      <ToolButton
        icon={ClipboardPasteIcon}
        label={t("Paste")}
        title={`${t("Paste")} (${shortcut(k.paste[0])})`}
        className={cn(icon, !canPaste && phoneHidden)}
        disabled={!canPaste}
        onClick={paste}
      />
      <ToolButton
        icon={PencilIcon}
        label={t("Rename")}
        title={`${t("Rename")} (${shortcut(k.rename[0])})`}
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
        title={`${t("Delete")} (${shortcut(k.trash[0])})`}
        className={cn(icon, phoneHidden)}
        disabled={none || !caps.del}
        onClick={() => setDialog({ t: "trash", picked: s.picked })}
      />
      <ToolSeparator className="max-md:hidden" />
      {p.sort && p.onSortChange && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<ToolButton icon={ArrowDownUpIcon} label={t("Sort")} showLabel phoneLabel className="h-9 px-2.5 text-[13px]" />} />
          <DropdownMenuContent className="w-44">
            <SortChoices sort={p.sort} onChange={p.onSortChange} />
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={LayoutListIcon} label={t("View")} showLabel phoneLabel className="h-9 px-2.5 text-[13px]" />} />
        <DropdownMenuContent className="w-52">
          <ViewChoices view={view} onChange={setView} />
          <DropdownMenuSeparator />
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <GroupIcon /> {t("Group by")}
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="w-44">
              <GroupChoices groupBy={groupBy} onChange={setGroupBy} />
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
            <SquareCheckIcon /> {t("Select all")} <Kbd>{k.selectAll[0]}</Kbd>
          </DropdownMenuItem>
          <DropdownMenuItem disabled={none} onClick={() => setSelected(new Set())}>
            <XSquareIcon /> {t("Select none")} <Kbd>{k.clearSelection[0]}</Kbd>
          </DropdownMenuItem>
          {/* With nothing selected, inverting selects everything (like File Explorer) */}
          <DropdownMenuItem disabled={!s.canInvert} onClick={s.invert}>
            <SquareCheckIcon /> {t("Invert selection")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={openShortcuts}>
            <KeyboardIcon /> {t("Keyboard shortcuts")} <Kbd>{k.shortcuts[0]}</Kbd>
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
        title={`${t("Details pane")} (${shortcut(k.details[0])})`}
        onClick={() => setDetailsOpen(!detailsOpen)}
      >
        <PanelRightIcon />
        <span className="max-lg:hidden">{t("Details pane")}</span>
      </Button>
    </>
  );

  return toolbar;
}
