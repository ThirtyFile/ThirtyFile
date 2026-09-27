import { createContext, useContext, useEffect, useState, useSyncExternalStore, type Dispatch, type KeyboardEvent, type SetStateAction } from "react";
import { Link, NavLink } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { ChevronRightIcon, CloudOffIcon, FolderIcon, FolderOpenIcon, LayersIcon, type LucideIcon } from "lucide-react";
import { api } from "@/api";
import { NavMenu } from "@/components/NavMenu";
import { DRIVE_ICON, useDrives } from "@/lib/drives";
import { t, tServer } from "@/lib/i18n";
import { cn } from "@/lib/utils";

// ───────────── Folder tree state (kept across page switches) ─────────────

let expanded = new Set<string>();
const treeListeners = new Set<() => void>();
function setExpanded(id: string, open: boolean) {
  if (expanded.has(id) === open) return;
  expanded = new Set(expanded);
  if (open) expanded.add(id);
  else expanded.delete(id);
  treeListeners.forEach((l) => l());
}
function useExpanded() {
  return useSyncExternalStore(
    (l) => {
      treeListeners.add(l);
      return () => {
        treeListeners.delete(l);
      };
    },
    () => expanded,
  );
}

/** When opening a folder, auto-expand its parent folders in the left-hand tree */
export function expandPath(ids: string[]) {
  ids.forEach((id) => setExpanded(id, true));
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

/** Arrow on the left of a row: mouse only (the keyboard uses ← and →), so it's hidden from screen readers and isn't a Tab stop */
function Expander({ id, open, hidden }: { id: string; open: boolean; hidden?: boolean }) {
  return (
    <span aria-hidden className={cn(expander, hidden && "invisible")} onClick={() => setExpanded(id, !open)}>
      <ChevronRightIcon className={cn("size-3.5 transition-transform", open && "rotate-90")} />
    </span>
  );
}

function TreeFolder({ id, name, depth, pos, size, activeId }: { id: string; name: string; depth: number; pos: number; size: number; activeId?: string }) {
  const open = useExpanded().has(id);
  const children = useQuery({
    queryKey: ["children", id, "folders"],
    queryFn: () => api.children(id, "name", "asc", true),
    enabled: open,
  });
  const empty = children.data?.length === 0;
  const item = useTreeItem(id, depth + 1, pos, size);
  return (
    <>
      <NavMenu to={`/files/${id}`} nodeId={id}>
        <div className={cn("group", row, activeId === id && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection")} style={{ paddingLeft: depth * 12 }}>
          <Expander id={id} open={open} hidden={empty} />
          <Link
            to={`/files/${id}`}
            {...item}
            aria-expanded={empty ? undefined : open}
            aria-current={activeId === id ? "page" : undefined}
            className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2 outline-none"
            title={name}
          >
            {open ? <FolderOpenIcon className="size-[15px] shrink-0" /> : <FolderIcon className="size-[15px] shrink-0" />}
            <span className="truncate">{name}</span>
          </Link>
        </div>
      </NavMenu>
      {open &&
        children.data?.map((c, i) => (
          <TreeFolder key={c.id} id={c.id} name={c.name} depth={depth + 1} pos={i + 1} size={children.data.length} activeId={activeId} />
        ))}
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
    queryKey: ["children", rootId, "folders"],
    queryFn: () => api.children(rootId, "name", "asc", true),
    enabled: open,
  });
  const empty = folders.data?.length === 0;
  const item = useTreeItem(rootId, depth + 1, pos, size);
  return (
    <>
      <NavMenu to={to} nodeId={rootId} isSpaceRoot>
        <div className={cn(row, activeId === rootId && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection")} style={{ paddingLeft: depth * 12 }}>
          <Expander id={rootId} open={open} hidden={empty} />
          <Link
            to={to}
            {...item}
            aria-expanded={empty ? undefined : open}
            aria-current={activeId === rootId ? "page" : undefined}
            className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2 outline-none"
            title={offline ? t("{name}: storage service offline ({reason}). You can browse, but you can't open, download, or upload files.", { name: label, reason: tServer(offline) }) : undefined}
          >
            <Icon className={cn("size-[15px] shrink-0", offline && "opacity-40")} />
            <span className="truncate">{label}</span>
            {offline && <CloudOffIcon className="ml-auto size-3.5 shrink-0 text-destructive" aria-label={t("Offline")} />}
          </Link>
        </div>
      </NavMenu>
      {open &&
        folders.data?.map((f, i) => (
          <TreeFolder key={f.id} id={f.id} name={f.name} depth={depth + 1} pos={i + 1} size={folders.data.length} activeId={activeId} />
        ))}
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
              cn("-ml-5 flex h-full min-w-0 flex-1 items-center gap-[7px] rounded pr-2 pl-5 outline-none", isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)]")
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
      <div role="tree" aria-label={t("Folders")} onKeyDown={onKeyDown}>
        <ThisPc activeId={activeId} />
      </div>
    </TabStop.Provider>
  );
}
