/**
 * The Mac style's sidebar: the same locations as the Windows style's navigation pane, in groups. Favorites (the list
 * of every favourite, the favourite folders and the smart folders, with New smart folder), Spaces, Shared (with me, my share links), Recent, Trash and
 * Tags; then the control panel for administrators, and the account menu. A group's heading hides or shows it (kept in
 * the browser). Folders and spaces take items and files dropped on them, as in the Windows style.
 */
import type { ReactNode } from "react";
import { NavLink } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { ChevronDownIcon, type LucideIcon } from "lucide-react";
import { api } from "@/api";
import { keys } from "@/api/queryKeys";
import { NavMenu } from "@/components/NavMenu";
import { Resizer } from "@/components/Resizer";
import { NAV_DEFAULT_WIDTH, NAV_MAX_WIDTH, NAV_MIN_WIDTH, NavFooter } from "@/components/frame/LocationsNav";
import { NewSmartFolderButton, SmartFolderItem } from "@/components/frame/SmartFoldersNav";
import { NewTagButton, TagItem } from "@/components/frame/TagsNav";
import { useFolderDrop } from "@/lib/dnd";
import { useDrives } from "@/lib/drives";
import { t } from "@/lib/i18n";
import { usePersisted, useMe } from "@/lib/session";
import { useSmartFolders } from "@/lib/smartFolders";
import { useTags } from "@/lib/tags";
import { cn } from "@/lib/utils";
import { useMacSymbols } from "./look";

/** A location's link: the one open is highlighted */
export const macNavItem = (isActive: boolean) =>
  cn(
    "flex h-(--mac-nav-item-h) items-center gap-2 rounded-(--tf-row-radius) px-2 text-(length:--mac-nav-text) whitespace-nowrap text-(--mac-nav-fg) outline-none hover:bg-(--mac-nav-hover) focus-visible:ring-(length:--tf-focus-w) focus-visible:ring-(--tf-focus) focus-visible:ring-inset",
    isActive && "bg-(--mac-nav-sel-bg) hover:bg-(--mac-nav-sel-bg)",
  );

/** The groups hidden from their heading */
const HIDDEN_KEY = "tf-mac-sidebar-hidden";

/** A group: its heading hides or shows what it holds; `action` is a button beside the heading */
function Group({ id, title, action, children }: { id: string; title: string; action?: ReactNode; children: ReactNode }) {
  const [hidden, setHidden] = usePersisted<string[]>(HIDDEN_KEY, []);
  const open = !hidden.includes(id);
  const heading = `mac-nav-${id}`;
  return (
    <section aria-labelledby={heading} className="group/section mt-3 first:mt-0">
      <div className="flex items-center gap-1 pr-1">
        <h2 id={heading} className="min-w-0 flex-1">
          <button
            type="button"
            aria-expanded={open}
            onClick={() => setHidden(open ? [...hidden, id] : hidden.filter((h) => h !== id))}
            className="flex w-full items-center gap-1 rounded px-2 py-0.5 text-left text-(length:--mac-nav-heading-text) font-semibold text-(--mac-nav-heading) outline-none hover:text-(--mac-nav-fg) focus-visible:ring-(length:--tf-focus-w) focus-visible:ring-(--tf-focus)"
          >
            <span className="truncate">{title}</span>
            <ChevronDownIcon
              aria-hidden
              className={cn("size-3 shrink-0 opacity-0 transition group-focus-within/section:opacity-100 group-hover/section:opacity-100", !open && "-rotate-90 opacity-100")}
            />
          </button>
        </h2>
        {action}
      </div>
      {open && <ul className="mt-0.5 grid gap-px">{children}</ul>}
    </section>
  );
}

/** A link to a page: Recent, Trash, Shared with me… */
function PageItem({ to, icon: Icon, label, end }: { to: string; icon: LucideIcon; label: string; end?: boolean }) {
  return (
    <li>
      <NavMenu to={to}>
        <NavLink to={to} end={end} className={({ isActive }) => macNavItem(isActive)}>
          <Icon className="size-4 shrink-0 text-(--mac-nav-symbol)" />
          <span className="truncate">{label}</span>
        </NavLink>
      </NavMenu>
    </li>
  );
}

/** A folder, or a space's top folder: a link that takes dropped items and files */
function FolderItem({ id, label, icon: Icon, isSpaceRoot, offline, current }: { id: string; label: string; icon: LucideIcon; isSpaceRoot?: boolean; offline?: boolean; current?: boolean }) {
  const { dropping, dropProps } = useFolderDrop({ id, name: label });
  const to = `/files/${id}`;
  return (
    <li {...dropProps}>
      <NavMenu to={to} nodeId={id} isSpaceRoot={isSpaceRoot}>
        <NavLink to={to} title={label} className={({ isActive }) => cn(macNavItem(isActive || !!current), dropping && "bg-brand/15 ring-1 ring-brand ring-inset")}>
          <Icon className={cn("size-4 shrink-0 text-(--mac-nav-symbol)", offline && "opacity-40")} />
          <span className="truncate">{label}</span>
        </NavLink>
      </NavMenu>
    </li>
  );
}

/** Favourite folders, in name order (the list of every favourite has files too) */
function FavoriteFolders({ activeFolder }: { activeFolder?: string }) {
  const sym = useMacSymbols();
  const favorites = useQuery({ queryKey: keys.favorites("name", "asc"), queryFn: () => api.favorites("name", "asc") });
  return favorites.data?.filter((n) => n.kind === "folder").map((n) => <FolderItem key={n.id} id={n.id} label={n.name} icon={sym.folder} current={n.id === activeFolder} />);
}

export function MacSidebar({ activeFolder }: { activeFolder?: string }) {
  const me = useMe();
  const drives = useDrives();
  const { tags } = useTags();
  const { folders: smart } = useSmartFolders();
  const [width, setWidth] = usePersisted("tf-nav-width", NAV_DEFAULT_WIDTH);
  const sym = useMacSymbols();
  const spaceIcon = { personal: sym.personal, company: sym.company, team: sym.team };
  return (
    <nav aria-label={t("File locations")} style={{ width, maxWidth: "40vw" }} className="tf-mac-sidebar relative m-2 mr-0 flex shrink-0 flex-col rounded-2xl">
      <Resizer width={width} onChange={setWidth} min={NAV_MIN_WIDTH} max={NAV_MAX_WIDTH} defaultWidth={NAV_DEFAULT_WIDTH} edge="right" label={t("Resize navigation pane")} />
      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3">
        <Group id="favorites" title={t("Favorites")} action={<NewSmartFolderButton />}>
          <PageItem to="/favorites" icon={sym.favorites} label={t("All favorites")} />
          <FavoriteFolders activeFolder={activeFolder} />
          {smart.map((folder) => (
            <li key={folder.id}>
              <SmartFolderItem folder={folder} itemClass={macNavItem} icon={sym.smartFolder} iconClass="size-4 text-(--mac-nav-symbol)" />
            </li>
          ))}
        </Group>
        <Group id="spaces" title={t("Spaces")}>
          <PageItem to="/drives" icon={sym.spaces} label={t("All spaces")} end />
          {drives.data?.map((d) => (
            <FolderItem key={d.id} id={d.root_id} label={d.name} icon={spaceIcon[d.kind]} isSpaceRoot offline={!!d.offline} current={d.root_id === activeFolder} />
          ))}
        </Group>
        <Group id="shared" title={t("Shared")}>
          <PageItem to="/shared-with-me" icon={sym.sharedWithMe} label={t("Shared with me")} />
          <PageItem to="/shares" icon={sym.shareLinks} label={t("My share links")} />
        </Group>
        <ul className="mt-3 grid gap-px">
          <PageItem to="/recent" icon={sym.recent} label={t("Recent")} />
          <PageItem to="/trash" icon={sym.trash} label={t("Trash")} />
        </ul>
        <Group id="tags" title={t("Tags")} action={<NewTagButton />}>
          {tags.map((tag) => (
            <li key={tag.id}>
              <TagItem tag={tag} itemClass={macNavItem} />
            </li>
          ))}
        </Group>
        {me.role === "admin" && (
          <Group id="admin" title={t("Administration")}>
            <PageItem to="/admin" icon={sym.controlPanel} label={t("Control panel")} />
          </Group>
        )}
      </div>
      <NavFooter />
    </nav>
  );
}
