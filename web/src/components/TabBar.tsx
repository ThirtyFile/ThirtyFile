import { useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  CircleIcon,
  ClockIcon,
  FileIcon,
  CopyPlusIcon,
  BuildingIcon,
  LayersIcon,
  UsersRoundIcon,
  FolderIcon,
  FolderOpenIcon,
  Link2Icon,
  PlusIcon,
  SearchIcon,
  SettingsIcon,
  StarIcon,
  Trash2Icon,
  XIcon,
  type LucideIcon,
} from "lucide-react";
import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { CONTROL_PANEL_ITEMS } from "@/lib/controlPanel";
import { FileIcon as TypeIcon } from "@/components/FileIcon";
import { useFolderDrop } from "@/lib/dnd";
import { folderOfPath, hasPersonal } from "@/lib/home";
import { useMe } from "@/lib/session";
import { hasDraft, useDraftsVersion } from "@/lib/drafts";
import { t } from "@/lib/i18n";
import { currentEntry, useTabActions, useTabsState, viewedFile, type Tab } from "@/tabs";
import { MAIN_ID } from "@/components/Frame";

const TAB_MIME = "application/x-thirtyfile-tab";

/** Decide the tab icon and default title from the URL (`personal`: the person has "My files", which `/files` opens) */
function describe(path: string, personal: boolean): { icon: LucideIcon; title: string } {
  const p = path.split("?")[0];
  if (p === "/files" || p === "/files/root") return personal ? { icon: FolderOpenIcon, title: t("My files") } : { icon: FolderIcon, title: t("Files") };
  if (p === "/files/shared") return { icon: BuildingIcon, title: t("All files") };
  if (p === "/drives") return { icon: LayersIcon, title: t("All spaces") };
  if (p === "/shared-with-me") return { icon: UsersRoundIcon, title: t("Shared with me") };
  if (p.startsWith("/files/")) return { icon: FolderIcon, title: t("Folder") };
  if (p === "/recent") return { icon: ClockIcon, title: t("Recent") };
  if (p === "/favorites") return { icon: StarIcon, title: t("Favorites") };
  if (p === "/search") return { icon: SearchIcon, title: t("Search") };
  if (p === "/shares") return { icon: Link2Icon, title: t("My share links") };
  if (p === "/trash") return { icon: Trash2Icon, title: t("Trash") };
  if (p === "/admin") return { icon: SettingsIcon, title: t("Control panel") };
  const cp = CONTROL_PANEL_ITEMS.find((i) => i.to === p);
  if (cp) return { icon: cp.icon, title: cp.title };
  if (p.startsWith("/view/")) return { icon: FileIcon, title: t("File") };
  return { icon: FolderIcon, title: t("Files") };
}

function TabItem({ tab, active, onlyOne }: { tab: Tab; active: boolean; onlyOne: boolean }) {
  const { activate, close, closeOthers, open, move } = useTabActions();
  const [dropping, setDropping] = useState(false);
  const path = currentEntry(tab);
  const personal = hasPersonal(useMe());
  const { icon: Icon, title: fallback } = describe(path, personal);
  // Folder and file names come from the page; other fixed pages always use their current name (to avoid reusing a title saved before a rename)
  const dynamic = path.startsWith("/files/") || path.startsWith("/view/") || path.startsWith("/search");
  const title = (dynamic && tab.title) || fallback;
  const file = viewedFile(tab);
  useDraftsVersion();
  const unsaved = !!file && hasDraft(file);
  // A tab showing a folder takes items dragged onto it, like that folder in the tree
  const folder = folderOfPath(path, personal);
  const { dropping: droppingItems, dropProps } = useFolderDrop(folder ? { id: folder, name: title } : null);
  // Asks first when there are unsaved changes
  const requestClose = () => close(tab.id);

  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger
          render={<div />}
          role="tab"
          aria-selected={active}
          aria-keyshortcuts="Delete"
          aria-controls={MAIN_ID}
          tabIndex={active ? 0 : -1}
          title={title}
          draggable
          onDragStart={(e) => {
            e.dataTransfer.setData(TAB_MIME, tab.id);
            e.dataTransfer.effectAllowed = "move";
          }}
          onDragOver={(e) => {
            if (!e.dataTransfer.types.includes(TAB_MIME)) return dropProps.onDragOver?.(e);
            e.preventDefault();
            setDropping(true);
          }}
          onDragLeave={(e) => {
            setDropping(false);
            dropProps.onDragLeave?.(e);
          }}
          onDrop={(e) => {
            setDropping(false);
            const from = e.dataTransfer.getData(TAB_MIME);
            if (from) move(from, tab.id);
            else dropProps.onDrop?.(e);
          }}
          onMouseDown={(e) => {
            if (e.button === 0) activate(tab.id);
          }}
          onAuxClick={(e) => {
            // Middle click closes the tab
            if (e.button === 1) {
              e.preventDefault();
              requestClose();
            }
          }}
          className={cn(
            "group relative flex h-8 max-w-[220px] min-w-[72px] flex-1 cursor-default items-center gap-2 rounded-t-lg pr-1.5 pl-3 text-xs select-none",
            active
              ? "bg-background text-foreground shadow-[0_-1px_0_var(--border),1px_0_0_var(--border),-1px_0_0_var(--border)]"
              : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            dropping && "ring-2 ring-brand ring-inset",
            droppingItems && "bg-brand/15 text-foreground ring-2 ring-brand ring-inset",
          )}
        >
          {file ? (
            <TypeIcon node={{ kind: "file", name: title, mime: "" }} className="size-3.5" />
          ) : (
            <Icon className={cn("size-3.5 shrink-0", Icon === FolderIcon || Icon === FolderOpenIcon ? "text-[#d8b66c]" : "")} />
          )}
          <span className="min-w-0 flex-1 truncate">{title}</span>
          {/* For the mouse: the keyboard closes the focused tab with Delete, so the button isn't a Tab stop (hidden on
              other tabs until pointed at, it would be an invisible one) and isn't read out inside the tab's name */}
          <button
            type="button"
            tabIndex={-1}
            aria-hidden
            title={unsaved ? t("Unsaved changes") : t("Close tab")}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              requestClose();
            }}
            className={cn(
              "flex size-5 shrink-0 items-center justify-center rounded hover:bg-accent",
              !active && !unsaved && "opacity-0 group-hover:opacity-100",
            )}
          >
            {/* Show a dot while unsaved; show close only on hover */}
            {unsaved ? (
              <>
                <CircleIcon className="size-2 fill-current group-hover:hidden" />
                <XIcon className="hidden size-3 group-hover:block" />
              </>
            ) : (
              <XIcon className="size-3" />
            )}
          </button>
          {/* Separator between unselected tabs */}
          {!active && <span className="absolute top-2 right-0 bottom-2 w-px bg-border group-hover:opacity-0" />}
        </ContextMenuTrigger>
        <ContextMenuContent>
          <DropdownMenuItem onClick={() => open(path)}>
            <CopyPlusIcon /> {t("Duplicate tab")}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => close(tab.id)}>
            <XIcon /> {t("Close tab")}
          </DropdownMenuItem>
          <DropdownMenuItem disabled={onlyOne} onClick={() => closeOthers(tab.id)}>
            {t("Close other tabs")}
          </DropdownMenuItem>
        </ContextMenuContent>
      </ContextMenu>
    </>
  );
}

export function TabBar() {
  const { tabs, active } = useTabsState();
  const { open, activate, close } = useTabActions();
  // Keyboard: Tab reaches the active tab, Left/Right (Home/End) switch to the others, Delete closes it
  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement).getAttribute("role") !== "tab" || e.altKey) return;
    const i = tabs.findIndex((t) => t.id === active);
    if (e.key === "Delete") {
      e.preventDefault();
      const bar = e.currentTarget;
      close(active);
      // The tab that takes its place gets the focus (not when closing waits for an answer about unsaved changes)
      requestAnimationFrame(() => {
        if (!document.querySelector("[role=dialog], [role=alertdialog]")) bar.querySelector<HTMLElement>("[role=tab][aria-selected=true]")?.focus();
      });
      return;
    }
    let next: number | null = null;
    if (e.key === "ArrowRight") next = (i + 1) % tabs.length;
    else if (e.key === "ArrowLeft") next = (i - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = tabs.length - 1;
    if (next === null || next === i) return;
    e.preventDefault();
    activate(tabs[next].id);
    const items = e.currentTarget.querySelectorAll<HTMLElement>("[role=tab]");
    items[next]?.focus();
  };
  return (
    <div
      role="tablist"
      aria-label={t("Tabs")}
      className="flex h-10 shrink-0 items-end gap-0.5 overflow-x-auto bg-sidebar px-2 pt-2"
      onKeyDown={onKeyDown}
      onDoubleClick={(e) => e.target === e.currentTarget && open()}
    >
      {tabs.map((tab) => (
        <TabItem key={tab.id} tab={tab} active={tab.id === active} onlyOne={tabs.length === 1} />
      ))}
      <button
        type="button"
        aria-label={t("New tab")}
        title={t("New tab")}
        onClick={() => open()}
        className="mb-1 ml-1 flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground"
      >
        <PlusIcon className="size-4" />
      </button>
    </div>
  );
}
