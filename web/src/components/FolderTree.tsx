import { createContext, useContext, useEffect, useRef, useState, type Dispatch, type KeyboardEvent, type SetStateAction } from "react";
import { Link, NavLink } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronRightIcon, ChevronsDownUpIcon, CloudOffIcon, FolderIcon, FolderOpenIcon, LayersIcon, LocateFixedIcon, type LucideIcon } from "lucide-react";
import { api, type Node, type NodeInfo } from "@/api";
import { keys } from "@/api/queryKeys";
import { useFolderDrop } from "@/lib/dnd";
import { NavMenu } from "@/components/NavMenu";
import { InlineRename } from "@/components/InlineRename";
import { useClickToRename } from "@/lib/clickToRename";
import { capsOf } from "@/lib/drives";
import { refreshFiles, renamed } from "@/lib/queries";
import { useMe } from "@/lib/session";
import { toastWithUndo } from "@/lib/undo";
import { useWindowsBehaviour } from "@/lib/windowsBehaviour";
import { ToolButton } from "@/components/frame/ToolButton";
import { DRIVE_ICON, useDrives } from "@/lib/drives";
import { t, tServer } from "@/lib/i18n";
import { treeStorageKey } from "@/lib/signOut";
import { createStore, useStore } from "@/lib/store";
import { cn } from "@/lib/utils";

// ───────────── Folder tree state (kept across page switches and reloads) ─────────────

/** At most this many expanded items are remembered: the most recently expanded ones (ids of deleted folders drop out over time) */
const TREE_STORED_MAX = 500;

function loadExpanded(key: string): string[] {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    if (!Array.isArray(stored)) return [];
    return stored.filter((id): id is string => typeof id === "string").slice(-TREE_STORED_MAX);
  } catch {
    // Storage blocked by the browser, or not ours to read: start with everything collapsed
    return [];
  }
}

/**
 * localStorage key of the signed-in user's expanded items (lib/signOut.ts: treeStorageKey), so the tree looks the
 * same after a reload; null until the user is known (items expanded before are kept when their tree is loaded)
 */
let treeKey: string | null = null;
const expanded = createStore(new Set<string>());

/** Loads the tree of the signed-in user, as they left it */
export function loadTree(userId: number) {
  const key = treeStorageKey(userId);
  if (key === treeKey) return;
  const early = treeKey === null ? [...expanded.get()] : [];
  treeKey = key;
  replaceExpanded(new Set([...loadExpanded(key), ...early]));
}

function replaceExpanded(next: Set<string>) {
  try {
    if (treeKey) localStorage.setItem(treeKey, JSON.stringify([...next].slice(-TREE_STORED_MAX)));
  } catch {
    // Storage blocked by the browser: the tree is remembered until the page is closed
  }
  expanded.set(next);
}
function setExpanded(id: string, open: boolean) {
  if (expanded.get().has(id) === open) return;
  const next = new Set(expanded.get());
  if (open) next.add(id);
  else next.delete(id);
  replaceExpanded(next);
}
function useExpanded() {
  return useStore(expanded);
}

/** When opening a folder, auto-expand its parent folders in the left-hand tree */
export function expandPath(ids: string[]) {
  ids.forEach((id) => setExpanded(id, true));
}

/** The tree items to expand for a folder to show: "All spaces", its space, and the folders above it; null when it isn't in the tree (reached through a share) */
export function treePathOf(info: NodeInfo): string[] | null {
  if (info.via_share) return null;
  return [ROOT, info.drive.root_id, ...info.path.slice(0, -1).map((c) => c.id)];
}

// ───────────── Keyboard (ARIA tree pattern) ─────────────

const ROOT = "this-pc";

/** The item reachable with Tab (roving tabindex): the tree is one Tab stop, arrows move inside it */
const TabStop = createContext<{ key: string; setKey: Dispatch<SetStateAction<string>> }>({ key: ROOT, setKey: () => {} });

/** Props of a tree item: its level and place among its siblings, and the roving tabindex */
function useTreeItem(key: string, level: number, pos: number, size: number) {
  const { key: tabKey, setKey } = useContext(TabStop);
  // An item hidden by collapsing its parent can't keep the Tab stop: it goes back to the root
  useEffect(() => () => setKey((k) => (k === key ? ROOT : k)), [key, setKey]);
  return {
    role: "treeitem",
    "data-tree-id": key,
    "aria-level": level,
    "aria-posinset": pos,
    "aria-setsize": size,
    tabIndex: tabKey === key ? 0 : -1,
    onFocus: () => setKey(key),
  };
}

const row = "flex h-[29px] items-center rounded text-muted-foreground hover:bg-muted has-[a:focus-visible]:ring-2 has-[a:focus-visible]:ring-ring";
const expander = "flex h-full w-5 shrink-0 items-center justify-center";

/** Items dragged over a collapsed folder for a moment open it, so they can be dropped deeper (like File Explorer) */
function useExpandOnHover(id: string, dropping: boolean, open: boolean) {
  useEffect(() => {
    if (!dropping || open) return;
    const timer = setTimeout(() => setExpanded(id, true), 800);
    return () => clearTimeout(timer);
  }, [id, dropping, open]);
}

const droppingRow = "bg-brand/15 text-foreground ring-1 ring-brand ring-inset";

/** Arrow on the left of a row: mouse only (the keyboard uses ← and →), so it's hidden from screen readers and isn't a Tab stop */
function Expander({ id, open, hidden }: { id: string; open: boolean; hidden?: boolean }) {
  return (
    <span aria-hidden className={cn(expander, hidden && "invisible")} onClick={() => setExpanded(id, !open)}>
      <ChevronRightIcon className={cn("size-3.5 transition-transform", open && "rotate-90")} />
    </span>
  );
}

/** Subfolders shown at first, and added with each "Show more": a folder of thousands would otherwise draw them all at once */
const TREE_PAGE = 100;

/** The subfolders of an expanded folder, the first TREE_PAGE of them until more are asked for */
function Subfolders({ parentId, folders, depth, activeId }: { parentId: string; folders: Node[]; depth: number; activeId?: string }) {
  const expandedIds = useExpanded();
  const [shown, setShown] = useState(TREE_PAGE);
  // The folder open on the right, and the folders expanded on the way to it, stay in view
  const kept = folders.findLastIndex((f) => f.id === activeId || expandedIds.has(f.id));
  const count = Math.max(shown, kept + 1);
  const more = useTreeItem(`${parentId}:more`, depth + 2, Math.min(count + 1, folders.length), folders.length);
  return (
    <>
      {folders.slice(0, count).map((f, i) => (
        <TreeFolder key={f.id} id={f.id} name={f.name} depth={depth + 1} pos={i + 1} size={folders.length} activeId={activeId} hasFolders={f.has_folders} />
      ))}
      {count < folders.length && (
        <button
          type="button"
          {...more}
          className={cn(row, "w-full pr-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring")}
          style={{ paddingLeft: (depth + 1) * 12 + 20 }}
          onClick={() => setShown(count + TREE_PAGE)}
        >
          {t("Show {n} more folders…", { n: Math.min(TREE_PAGE, folders.length - count) })}
        </button>
      )}
    </>
  );
}

/** The folder whose name is being edited in the tree (clicking the open folder's name, as in File Explorer) */
const treeRenaming = createStore<string | null>(null);

/** The name box of a folder renamed in the tree; Enter or Esc puts the focus back on the folder */
function TreeRename({ id, name }: { id: string; name: string }) {
  const qc = useQueryClient();
  return (
    <InlineRename
      initial={name}
      selectAll
      onSubmit={async (next) => {
        void refreshFiles(qc, renamed(await api.rename(id, next)));
        toastWithUndo(t('Renamed to "{name}"', { name: next }), {
          undo: async () => void refreshFiles(qc, renamed(await api.rename(id, name))),
          undoneText: t("Renamed back"),
          label: t("Undo rename"),
        });
      }}
      onDone={(byKey) => {
        treeRenaming.set(null);
        if (byKey) requestAnimationFrame(() => document.querySelector<HTMLElement>(`[role="tree"] [data-tree-id="${CSS.escape(id)}"]`)?.focus());
      }}
    />
  );
}

function TreeFolder({
  id,
  name,
  depth,
  pos,
  size,
  activeId,
  hasFolders,
}: {
  id: string;
  name: string;
  depth: number;
  pos: number;
  size: number;
  activeId?: string;
  /** From the parent's listing: whether it has folders in it (unknown: an arrow until it is expanded) */
  hasFolders?: boolean;
}) {
  const open = useExpanded().has(id);
  const children = useQuery({
    queryKey: keys.folders(id),
    queryFn: ({ signal }) => api.children(id, "name", "asc", true, signal),
    enabled: open,
  });
  // No folders in it: no arrow to expand (its listing, once loaded, is what counts)
  const empty = children.data ? children.data.length === 0 : hasFolders === false;
  const item = useTreeItem(id, depth + 1, pos, size);
  const { dropping, dropProps } = useFolderDrop({ id, name });
  useExpandOnHover(id, dropping, open);
  const renaming = useStore(treeRenaming) === id;
  const icon = open ? <FolderOpenIcon className="size-[15px] shrink-0" /> : <FolderIcon className="size-[15px] shrink-0" />;
  return (
    <>
      <NavMenu to={`/files/${id}`} nodeId={id}>
        <div
          {...dropProps}
          className={cn("group", row, activeId === id && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection", dropping && droppingRow)}
          style={{ paddingLeft: depth * 12 }}
        >
          <Expander id={id} open={open} hidden={empty} />
          {renaming ? (
            <div className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2">
              {icon}
              <TreeRename id={id} name={name} />
            </div>
          ) : (
            <Link
              to={`/files/${id}`}
              {...item}
              aria-expanded={empty ? undefined : open}
              aria-current={activeId === id ? "page" : undefined}
              className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2 outline-none"
              title={name}
            >
              {icon}
              <span data-name className="truncate">
                {name}
              </span>
            </Link>
          )}
        </div>
      </NavMenu>
      {open && children.data && <Subfolders parentId={id} folders={children.data} depth={depth} activeId={activeId} />}
    </>
  );
}

/** Space root; can be expanded to show the first level of folders */
function SpaceRoot({
  rootId,
  to,
  icon: Icon,
  label,
  activeId,
  depth = 0,
  pos,
  size,
  offline,
}: {
  rootId: string;
  to: string;
  icon: LucideIcon;
  label: string;
  activeId?: string;
  depth?: number;
  pos: number;
  size: number;
  /** Why the storage service is offline */
  offline?: string | null;
}) {
  const open = useExpanded().has(rootId);
  const folders = useQuery({
    queryKey: keys.folders(rootId),
    queryFn: ({ signal }) => api.children(rootId, "name", "asc", true, signal),
    enabled: open,
  });
  const empty = folders.data?.length === 0;
  const item = useTreeItem(rootId, depth + 1, pos, size);
  const { dropping, dropProps } = useFolderDrop({ id: rootId, name: label });
  useExpandOnHover(rootId, dropping, open);
  return (
    <>
      <NavMenu to={to} nodeId={rootId} isSpaceRoot>
        <div
          {...dropProps}
          className={cn(row, activeId === rootId && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection", dropping && droppingRow)}
          style={{ paddingLeft: depth * 12 }}
        >
          <Expander id={rootId} open={open} hidden={empty} />
          <Link
            to={to}
            {...item}
            aria-expanded={empty ? undefined : open}
            aria-current={activeId === rootId ? "page" : undefined}
            className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2 outline-none"
            title={
              offline ? t("{name}: storage service offline ({reason}). You can browse, but you can't open, download, or upload files.", { name: label, reason: tServer(offline) }) : undefined
            }
          >
            <Icon className={cn("size-[15px] shrink-0", offline && "opacity-40")} />
            <span className="truncate">{label}</span>
            {offline && <CloudOffIcon className="ml-auto size-3.5 shrink-0 text-destructive" aria-label={t("Offline")} />}
          </Link>
        </div>
      </NavMenu>
      {open && folders.data && <Subfolders parentId={rootId} folders={folders.data} depth={depth} activeId={activeId} />}
    </>
  );
}

/** "All spaces": can be expanded to list every space I can access */
function ThisPc({ activeId }: { activeId?: string }) {
  const open = useExpanded().has(ROOT);
  const drives = useDrives();
  const item = useTreeItem(ROOT, 1, 1, 1);
  return (
    <>
      <NavMenu to="/drives">
        <div className={row}>
          <Expander id={ROOT} open={open} />
          <NavLink
            to="/drives"
            end
            {...item}
            aria-expanded={open}
            className={({ isActive }) =>
              cn(
                "-ml-5 flex h-full min-w-0 flex-1 items-center gap-[7px] rounded pr-2 pl-5 outline-none",
                isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)]",
              )
            }
          >
            <LayersIcon className="size-[15px] shrink-0" />
            <span className="truncate">{t("All spaces")}</span>
          </NavLink>
        </div>
      </NavMenu>
      {open &&
        drives.data?.map((d, i) => (
          <SpaceRoot
            key={d.id}
            rootId={d.root_id}
            to={`/files/${d.root_id}`}
            icon={DRIVE_ICON[d.kind]}
            label={d.name}
            activeId={activeId}
            depth={1}
            pos={i + 1}
            size={drives.data.length}
            offline={d.offline}
          />
        ))}
    </>
  );
}

/**
 * The navigation pane's folder tree ("All spaces", its spaces and their folders), following the ARIA tree pattern:
 * one Tab stop, ↑/↓ move, → expands (or moves to the first child), ← collapses (or moves to the parent), Home/End jump, Enter opens.
 */
export function FolderTree({ activeId }: { activeId?: string }) {
  const [tabKey, setTabKey] = useState(ROOT);
  const tree = useRef<HTMLDivElement>(null);

  // Clicking the open folder's name renames it (the Windows style), where the person may: not a space's top folder.
  // Its details are the folder page's, which usually has them already
  const behaviour = useWindowsBehaviour();
  const me = useMe();
  const info = useQuery({ queryKey: keys.node(activeId), queryFn: () => api.node(activeId!), enabled: !!activeId && behaviour.clickToRename });
  const active = info.data?.node.id === activeId ? info.data : undefined;
  const canRename = !!active?.node.parent_id && capsOf(active.role, me, active.read_only).write;
  useClickToRename({
    enabled: behaviour.clickToRename && canRename,
    root: tree,
    itemOf: (el) => el.closest<HTMLElement>("[data-tree-id]")?.dataset.treeId ?? null,
    selectedAlone: (id) => id === activeId,
    canRename: (id) => id === activeId && canRename,
    start: (id) => treeRenaming.set(id),
  });

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const item = (e.target as HTMLElement).closest<HTMLElement>('[role="treeitem"]');
    if (!item || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    // Items as shown (children of collapsed items aren't rendered)
    const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('[role="treeitem"]'));
    const i = items.indexOf(item);
    const level = (el: HTMLElement) => Number(el.getAttribute("aria-level"));
    const state = item.getAttribute("aria-expanded");
    let target: HTMLElement | undefined;
    if (e.key === "ArrowDown") target = items[i + 1];
    else if (e.key === "ArrowUp") target = items[i - 1];
    else if (e.key === "Home") target = items[0];
    else if (e.key === "End") target = items.at(-1);
    else if (e.key === "ArrowRight") {
      if (state === "false") setExpanded(item.dataset.treeId!, true);
      else if (state === "true" && items[i + 1] && level(items[i + 1]) > level(item)) target = items[i + 1];
    } else if (e.key === "ArrowLeft") {
      if (state === "true") setExpanded(item.dataset.treeId!, false);
      else target = items.slice(0, i).findLast((x) => level(x) < level(item));
    } else return;
    e.preventDefault();
    target?.focus();
  };

  return (
    <TabStop.Provider value={{ key: tabKey, setKey: setTabKey }}>
      <div ref={tree} role="tree" aria-label={t("Folders")} onKeyDown={onKeyDown}>
        <ThisPc activeId={activeId} />
      </div>
    </TabStop.Provider>
  );
}

// ───────────── Toolbar above the tree ─────────────

/** How long "Show current folder" waits for the folders on the way to load before giving up */
const REVEAL_TIMEOUT = 5000;

/** Once the row of `id` is drawn (its parents may still be loading), scroll it into view and give it the focus */
function revealRow(scope: Element, id: string) {
  const deadline = performance.now() + REVEAL_TIMEOUT;
  const look = () => {
    const item = scope.querySelector<HTMLElement>(`[role="tree"] [data-tree-id="${CSS.escape(id)}"]`);
    if (item) {
      item.scrollIntoView({ block: "nearest" });
      item.focus({ preventScroll: true });
    } else if (performance.now() < deadline) requestAnimationFrame(look);
  };
  look();
}

/**
 * Buttons for the folder tree, kept outside `role="tree"` (and outside the scrolling list, so they stay in place):
 * Show current folder, and Collapse all (which leaves "All spaces" open, so the spaces stay listed).
 * A tree item that disappears when collapsing hands the tree's Tab stop back to "All spaces" (see useTreeItem).
 */
export function FolderTreeToolbar({ activeId }: { activeId?: string }) {
  // Shares the query of the folder page, which usually has it already
  const info = useQuery({ queryKey: keys.node(activeId), queryFn: () => api.node(activeId!), enabled: !!activeId });
  const path = activeId && info.data?.node.id === activeId ? treePathOf(info.data) : null;
  const button = "size-7 px-0";
  return (
    <div className="flex shrink-0 items-center justify-end gap-0.5 border-b px-1.5 py-1">
      <ToolButton
        icon={LocateFixedIcon}
        label={t("Show current folder")}
        className={button}
        disabled={!path}
        onClick={(e) => {
          if (!path || !activeId) return;
          expandPath(path);
          const nav = e.currentTarget.closest("nav");
          if (nav) revealRow(nav, activeId);
        }}
      />
      <ToolButton icon={ChevronsDownUpIcon} label={t("Collapse all")} className={button} onClick={() => replaceExpanded(new Set([ROOT]))} />
    </div>
  );
}
