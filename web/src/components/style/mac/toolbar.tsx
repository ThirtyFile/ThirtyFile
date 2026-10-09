/**
 * The Mac style's toolbar, as a Finder window's: the view (Icons, List, Columns, Gallery), how items are sorted and grouped,
 * Share, and an actions menu with what can be done here and with the items selected. Back, forward and the search box
 * are the frame's. Phones get the toolbar every style shares (the Windows style's).
 */
import { useContext, type ReactNode } from "react";
import {
  ClipboardPasteIcon,
  ChevronDownIcon,
  LayoutGridIcon,
  TagIcon,
  Columns3Icon,
  CopyIcon,
  EyeIcon,
  FolderInputIcon,
  FolderOpenIcon,
  FolderSymlinkIcon,
  InfoIcon,
  KeyboardIcon,
  PanelRightIcon,
  PencilIcon,
  Share2Icon,
  SquareCheckIcon,
  Trash2Icon,
  UsersRoundIcon,
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
import { groupable } from "@/components/fileList/layout";
import { ToolButton } from "@/components/frame/ToolButton";
import { openShortcuts } from "@/components/ShortcutsDialog";
import { useViews } from "@/components/style";
import { TagSubmenu, TagMenuItems } from "@/components/tags";
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
import { useMacSymbols } from "./look";
import { openQuickLook } from "./quickLook";
import { CompactToolbar, useMacWindowPrefs } from "./windowPrefs";
import { toolbarCommands } from "@/components/explorer/menus";

export function macToolbar(p: ExplorerProps, s: ExplorerState, a: ExplorerActions, newItems: ReactNode) {
  return <MacToolbar p={p} s={s} a={a} newItems={newItems} />;
}

/** An icon button of the toolbar: square, its size and look the style's (--tf-tool-*, art/mac.css) */
const icon = "tf-mac-capsule w-(--tf-tool-h) shrink-0 px-0";

function MacToolbar({ p, s, a, newItems }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; newItems: ReactNode }) {
  const compact = useContext(CompactToolbar);
  const phone = useMediaQuery("(max-width: 47.99rem)");
  if (phone) return windowsToolbar(p, s, a, newItems);
  return (
    <>
      <ViewSwitcher p={p} s={s} />
      {!compact && (
        <>
          <SortMenu p={p} />
          <GroupMenu s={s} />
          <ShareMenu s={s} />
          <TagsMenu s={s} />
        </>
      )}
      <ActionsMenu p={p} s={s} a={a} newItems={newItems} />
    </>
  );
}

/** The views side by side, the one shown pressed */
function ViewSwitcher({ p, s }: { p: ExplorerProps; s: ExplorerState }) {
  const views = useViews();
  const compact = useContext(CompactToolbar);
  const ActiveIcon = views.find((v) => v.id === s.view)?.Icon ?? LayoutGridIcon;
  if (compact)
    return (
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <ToolButton icon={ActiveIcon} label={t("View")} className="tf-mac-capsule shrink-0 px-2">
              <ChevronDownIcon className="size-3!" />
            </ToolButton>
          }
        />
        <DropdownMenuContent className="w-56">
          {views.map(({ id, Icon, label }) => (
            <DropdownMenuCheckboxItem key={id} checked={s.view === id} onCheckedChange={() => s.setView(id)} closeOnClick>
              <Icon /> {label}
            </DropdownMenuCheckboxItem>
          ))}
          <DropdownMenuSeparator />
          <ViewOptions p={p} s={s} />
        </DropdownMenuContent>
      </DropdownMenu>
    );
  return (
    <div role="group" aria-label={t("View")} className="tf-mac-capsule flex shrink-0 items-center p-0.5">
      {views.map(({ id, Icon, label }) => (
        <Button
          key={id}
          variant="ghost"
          aria-label={label}
          title={label}
          aria-pressed={s.view === id}
          className={cn(
            "h-6 w-8 rounded-[5px] px-0 text-(--tf-tool-fg) hover:bg-(--tf-tool-hover) dark:hover:bg-(--tf-tool-hover) [&_svg]:size-4",
            s.view === id && "bg-(--mac-seg-on) text-foreground shadow-(--mac-seg-shadow) hover:bg-(--mac-seg-on) dark:hover:bg-(--mac-seg-on)",
          )}
          onClick={() => s.setView(id)}
        >
          <Icon />
        </Button>
      ))}
      <DropdownMenu>
        <DropdownMenuTrigger render={<ToolButton icon={ChevronDownIcon} label={t("View options")} className="w-5 px-0" />} />
        <DropdownMenuContent className="w-56">
          <ViewOptions p={p} s={s} />
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/** Sorting and grouping are independent controls. */
function SortMenu({ p }: { p: ExplorerProps }) {
  const sym = useMacSymbols();
  if (!p.sort || !p.onSortChange) return null;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={sym.sort} label={t("Sort by")} className={icon} />} />
      <DropdownMenuContent className="w-52">
        <SortChoices sort={p.sort} onChange={p.onSortChange} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function GroupMenu({ s }: { s: ExplorerState }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <ToolButton icon={LayoutGridIcon} label={t("Group by")} disabled={!groupable(s.view)} className="tf-mac-capsule shrink-0 gap-0.5 px-2">
            <ChevronDownIcon className="size-3!" />
          </ToolButton>
        }
      />
      <DropdownMenuContent className="w-44">
        <GroupChoices groupBy={s.groupBy} onChange={s.setGroupBy} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function TagsMenu({ s }: { s: ExplorerState }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={TagIcon} label={t("Tags")} disabled={s.count === 0} className={icon} />} />
      <DropdownMenuContent className="max-h-80 w-56 overflow-y-auto">
        <TagMenuItems nodes={s.selectedNodes} picked={s.picked} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ViewOptions({ p, s }: { p: ExplorerProps; s: ExplorerState }) {
  const [bars, setBars] = useMacWindowPrefs();
  return (
    <>
      <DropdownMenuSub>
        <DropdownMenuSubTrigger disabled={s.view !== "list"}>
          <Columns3Icon /> {t("Columns")}
        </DropdownMenuSubTrigger>
        <DropdownMenuSubContent className="w-52">
          <ColumnChoices columns={listColumns(p, true)} />
        </DropdownMenuSubContent>
      </DropdownMenuSub>
      <DropdownMenuCheckboxItem checked={bars.path} onCheckedChange={(path) => setBars({ ...bars, path })} closeOnClick>
        {t("Show path bar")}
      </DropdownMenuCheckboxItem>
      <DropdownMenuCheckboxItem checked={bars.status} onCheckedChange={(status) => setBars({ ...bars, status })} closeOnClick>
        {t("Show status bar")}
      </DropdownMenuCheckboxItem>
      <DropdownMenuCheckboxItem checked={s.detailsOpen} onCheckedChange={s.setDetailsOpen} closeOnClick>
        <PanelRightIcon /> {t("Details pane")}
      </DropdownMenuCheckboxItem>
    </>
  );
}

function ShareChoices({ s }: { s: ExplorerState }) {
  const { single, caps, setDialog } = s;
  return (
    <>
      <DropdownMenuItem disabled={!single} onClick={() => single && setDialog({ t: "access", nodeId: single.id })}>
        <UsersRoundIcon /> {t("Share with…")}
      </DropdownMenuItem>
      <DropdownMenuItem disabled={!single || !caps.share} onClick={() => single && setDialog({ t: "share", node: single })}>
        <Share2Icon /> {t("Create share link")}
      </DropdownMenuItem>
    </>
  );
}

/** Sharing the item selected: with people, or with a link */
function ShareMenu({ s }: { s: ExplorerState }) {
  const sym = useMacSymbols();
  const { single } = s;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={sym.share} label={t("Share")} className={icon} disabled={!single} />} />
      <DropdownMenuContent className="w-48">
        <ShareChoices s={s} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** What can be made here, then what can be done with the items selected, then the window's own settings */
function ActionsMenu({ p, s, a, newItems }: { p: ExplorerProps; s: ExplorerState; a: ExplorerActions; newItems: ReactNode }) {
  const { caps, single, setDialog, setDetailsOpen } = s;
  const compact = useContext(CompactToolbar);
  const none = s.count === 0;
  const commands = toolbarCommands(p, s, a);
  const k = s.kit.keys;
  const sym = useMacSymbols();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={<ToolButton icon={sym.actions} label={t("Actions")} className={icon} />} />
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
        <DropdownMenuItem disabled={none} onClick={openQuickLook}>
          <EyeIcon /> {t("Quick look")} <Kbd>{k.quickLook[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={none} onClick={() => setDetailsOpen(true)}>
          <InfoIcon /> {t("Get info")} <Kbd>{k.details[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!single || !caps.write} onClick={() => single && setDialog({ t: "rename", node: single })}>
          <PencilIcon /> {t("Rename")} <Kbd>{k.rename[0]}</Kbd>
        </DropdownMenuItem>
        {commands.download}
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled={none} onClick={a.copy}>
          <CopyIcon /> {t("Copy")} <Kbd>{k.copy[0]}</Kbd>
        </DropdownMenuItem>
        {s.canCreate && (
          <DropdownMenuItem disabled={!a.canPaste} onClick={a.paste}>
            <ClipboardPasteIcon /> {t("Paste")} <Kbd>{k.paste[0]}</Kbd>
          </DropdownMenuItem>
        )}
        {s.canCreate && (
          <DropdownMenuItem disabled={!a.canPaste} onClick={() => void a.moveHere()}>
            <FolderInputIcon /> {t("Move here")} <Kbd>{k.moveHere[0]}</Kbd>
          </DropdownMenuItem>
        )}
        {commands.moveTo}
        {commands.copyTo}
        <DropdownMenuItem variant="destructive" disabled={none || !caps.del} onClick={() => setDialog({ t: "trash", picked: s.picked })}>
          <Trash2Icon /> {t("Move to trash")} <Kbd>{k.trash[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        {commands.favorite}
        {!none && <TagSubmenu nodes={s.selectedNodes} picked={s.picked} />}
        {commands.access}
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={openGoToFolder}>
          <FolderSymlinkIcon /> {t("Go to folder…")} <Kbd>{k.addressBar[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuItem onClick={s.selectAll}>
          <SquareCheckIcon /> {t("Select all")} <Kbd>{k.selectAll[0]}</Kbd>
        </DropdownMenuItem>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>{t("View")}</DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-56">
            <ViewOptions p={p} s={s} />
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        {compact && (
          <>
            {p.sort && p.onSortChange && (
              <DropdownMenuSub>
                <DropdownMenuSubTrigger>{t("Sort by")}</DropdownMenuSubTrigger>
                <DropdownMenuSubContent className="w-52">
                  <SortChoices sort={p.sort} onChange={p.onSortChange} />
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            )}
            <DropdownMenuSub>
              <DropdownMenuSubTrigger disabled={!groupable(s.view)}>{t("Group by")}</DropdownMenuSubTrigger>
              <DropdownMenuSubContent className="w-44">
                <GroupChoices groupBy={s.groupBy} onChange={s.setGroupBy} />
              </DropdownMenuSubContent>
            </DropdownMenuSub>
            <DropdownMenuSub>
              <DropdownMenuSubTrigger disabled={!single}>{t("Share")}</DropdownMenuSubTrigger>
              <DropdownMenuSubContent className="w-48">
                <ShareChoices s={s} />
              </DropdownMenuSubContent>
            </DropdownMenuSub>
          </>
        )}
        <DropdownMenuItem onClick={openShortcuts}>
          <KeyboardIcon /> {t("Keyboard shortcuts")} <Kbd>{k.shortcuts[0]}</Kbd>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
