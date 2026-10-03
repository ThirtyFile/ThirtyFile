/**
 * The Mac style's toolbar, as a Finder window's: the view (Icons, List, Columns), how items are sorted and grouped,
 * Share, and an actions menu with what can be done here and with the items selected. Back, forward and the search box
 * are the frame's. Phones get the toolbar every style shares (the Windows style's).
 */
import type { ReactNode } from "react";
import {
  ArrowDownUpIcon,
  ClipboardPasteIcon,
  Columns3Icon,
  CopyIcon,
  DownloadIcon,
  EllipsisIcon,
  FolderInputIcon,
  FolderOpenIcon,
  FolderSymlinkIcon,
  InfoIcon,
  KeyboardIcon,
  PanelRightIcon,
  PencilIcon,
  Share2Icon,
  SquareCheckIcon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
  UsersRoundIcon,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ColumnChoices, listColumns } from "@/components/fileList/columns";
import { ToolButton } from "@/components/frame/ToolButton";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { useViews } from "@/components/style";
import { TagSubmenu } from "@/components/tags";
import type { ExplorerProps } from "@/components/Explorer";
import type { ExplorerActions } from "@/components/explorer/actions";
import type { ExplorerState } from "@/components/explorer/state";
import { Kbd } from "@/components/explorer/ui";
import { GroupChoices, SortChoices } from "@/components/explorer/viewChoices";
import { useMediaQuery } from "@/lib/focus";
import { t } from "@/lib/i18n";
import { cn } from "@/lib/utils";
import { windowsToolbar } from "../windows/toolbar";
import { openGoToFolder } from "./pathBar";

export function macToolbar(p: ExplorerProps, s: ExplorerState, a: ExplorerActions, newItems: ReactNode) {
  return <MacToolbar p={p} s={s} a={a} newItems={newItems} />;
}

const icon = "size-8 px-0 [&_svg]:size-[18px]";

function MacToolbar({ p, s, a, newItems }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; newItems: ReactNode }) {
  const phone = useMediaQuery("(max-width: 47.99rem)");
  if (phone) return windowsToolbar(p, s, a, newItems);
  return (
    <>
      <ViewSwitcher s={s} />
      <SortAndGroup p={p} s={s} />
      <ShareMenu s={s} />
      <ActionsMenu p={p} s={s} a={a} newItems={newItems} />
    </>
  );
}

/** The views side by side, the one shown pressed */
function ViewSwitcher({ s }: { s: ExplorerState }) {
  const views = useViews();
  return (
    <div role="group" aria-label={t("View")} className="mr-1 flex items-center rounded-md bg-muted/70 p-0.5">
      {views.map(({ id, Icon, label }) => (
        <Button
          key={id}
          variant="ghost"
          aria-label={label}
          title={label}
          aria-pressed={s.view === id}
          className={cn("h-7 w-8 rounded-[5px] px-0 [&_svg]:size-4", s.view === id && "bg-background shadow-sm hover:bg-background")}
          onClick={() => s.setView(id)}
        >
          <Icon />
        </Button>
      ))}
    </div>
  );
}

/** How the items are sorted, then grouped (the Columns view doesn't group) */
function SortAndGroup({ p, s }: { p: ExplorerProps; s: ExplorerState }) {
  if (!p.sort || !p.onSortChange) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={ArrowDownUpIcon} label={t("Sort and group")} className={icon} />} />
      <DropdownMenuContent className="w-52">
        <DropdownMenuGroup>
          <DropdownMenuLabel>{t("Sort by")}</DropdownMenuLabel>
          <SortChoices sort={p.sort} onChange={p.onSortChange} />
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuSub>
          <DropdownMenuSubTrigger disabled={s.view === "columns"}>{t("Group by")}</DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-44">
            <GroupChoices groupBy={s.groupBy} onChange={s.setGroupBy} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Sharing the item selected: with people, or with a link */
function ShareMenu({ s }: { s: ExplorerState }) {
  const { single, caps, setDialog } = s;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={Share2Icon} label={t("Share")} className={icon} disabled={!single} />} />
      <DropdownMenuContent className="w-48">
        <DropdownMenuItem onClick={() => single && setDialog({ t: "access", nodeId: single.id })}>
          <UsersRoundIcon /> {t("Share with…")}
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!caps.share} onClick={() => single && setDialog({ t: "share", node: single })}>
          <Share2Icon /> {t("Create share link")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** What can be made here, then what can be done with the items selected, then the window's own settings */
function ActionsMenu({ p, s, a, newItems }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; newItems: ReactNode }) {
  const { caps, single, allFavorite, setDialog, detailsOpen, setDetailsOpen, view } = s;
  const none = s.count === 0;
  const k = s.kit.keys;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={EllipsisIcon} label={t("Actions")} className={icon} />} />
      <DropdownMenuContent className="w-60">
        {s.canCreate && (
          <>
            {newItems}
            <DropdownMenuSeparator />
          </>
        )}
        <DropdownMenuItem disabled={!single} onClick={() => single && a.open(single)}>
          <FolderOpenIcon /> {t("Open")} <Kbd>{k.open[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={none} onClick={() => setDetailsOpen(true)}>
          <InfoIcon /> {t("Get info")} <Kbd>{k.details[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!single || !caps.write} onClick={() => single && setDialog({ t: "rename", node: single })}>
          <PencilIcon /> {t("Rename")} <Kbd>{k.rename[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={none} onClick={() => a.download(s.picked)}>
          <DownloadIcon /> {t("Download")}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled={none} onClick={a.copy}>
          <CopyIcon /> {t("Copy")} <Kbd>{k.copy[0]}</Kbd>
        </DropdownMenuItem>
        {s.canCreate && (
          <DropdownMenuItem disabled={!a.canPaste} onClick={a.paste}>
            <ClipboardPasteIcon /> {t("Paste")} <Kbd>{k.paste[0]}</Kbd>
          </DropdownMenuItem>
        )}
        <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "move", picked: s.picked })}>
          <FolderInputIcon /> {t("Move to…")}
        </DropdownMenuItem>
        <DropdownMenuItem disabled={none || !caps.write} onClick={() => setDialog({ t: "copy", picked: s.picked })}>
          <CopyIcon /> {t("Copy to…")}
        </DropdownMenuItem>
        <DropdownMenuItem variant="destructive" disabled={none || !caps.del} onClick={() => setDialog({ t: "trash", picked: s.picked })}>
          <Trash2Icon /> {t("Move to trash")} <Kbd>{k.trash[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled={none} onClick={a.toggleFavorite}>
          {allFavorite ? <StarOffIcon /> : <StarIcon />} {allFavorite ? t("Remove from favorites") : t("Add to favorites")}
        </DropdownMenuItem>
        {!none && <TagSubmenu nodes={s.selectedNodes} picked={s.picked} />}
        {p.folderId && (
          <DropdownMenuItem onClick={() => setDialog({ t: "access", nodeId: p.folderId! })}>
            <UsersRoundIcon /> {p.folder?.parent_id ? t("Access to this folder") : t("Space members")}
          </DropdownMenuItem>
        )}
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={openGoToFolder}>
          <FolderSymlinkIcon /> {t("Go to folder…")} <Kbd>{k.addressBar[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem onClick={s.selectAll}>
          <SquareCheckIcon /> {t("Select all")} <Kbd>{k.selectAll[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger disabled={view !== "list"}>
            <Columns3Icon /> {t("Columns")}
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-52">
            <ColumnChoices columns={listColumns(p)} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuCheckboxItem checked={detailsOpen} onCheckedChange={(on) => setDetailsOpen(on)} closeOnClick>
          <PanelRightIcon /> {t("Details pane")}
        </DropdownMenuCheckboxItem>
        <DropdownMenuItem onClick={openShortcuts}>
          <KeyboardIcon /> {t("Keyboard shortcuts")} <Kbd>{k.shortcuts[0]}</Kbd>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
