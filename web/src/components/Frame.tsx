import { useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { leaveAfterSignOut } from "@/lib/signOut";
import { Link, NavLink, useLocation, useNavigate, useSearchParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeftIcon,
  ArrowRightIcon,
  ArrowUpIcon,
  CheckIcon,
  ChevronRightIcon,
  ChevronsUpDownIcon,
  ClockIcon,
  CloudOffIcon,
  CopyIcon,
  LayersIcon,
  UsersRoundIcon,
  FolderIcon,
  FolderOpenIcon,
  HistoryIcon,
  KeyRoundIcon,
  LanguagesIcon,
  Link2Icon,
  LogOutIcon,
  MoonIcon,
  PanelLeftIcon,
  RefreshCwIcon,
  SearchIcon,
  SettingsIcon,
  StarIcon,
  SunIcon,
  Trash2Icon,
  type LucideIcon,
} from "lucide-react";
import { api } from "@/api";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
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
import { Input } from "@/components/ui/input";
import { ChangePasswordDialog } from "@/components/dialogs";
import { LoginLogDialog } from "@/components/logs/LoginLog";
import { LinkedAccountsDialog } from "@/components/LinkedAccountsDialog";
import { NavMenu } from "@/components/NavMenu";
import { Resizer } from "@/components/Resizer";
import { DRIVE_ICON, useDrives } from "@/lib/drives";
import { useMediaQuery, useOverlayFocus } from "@/lib/focus";
import { usePersisted, useMe } from "@/lib/session";
import { useTheme, type ThemeMode } from "@/lib/theme";
import { useBranding } from "@/lib/branding";
import { LANGS, lang, setLang, t, tServer, type Lang } from "@/lib/i18n";
import { cn, copyText, formatBytes } from "@/lib/utils";
import { setActiveTitle, useTabActions, useTabsState } from "@/tabs";

// ───────────── Toolbar buttons ─────────────

export function ToolButton({
  icon: Icon,
  label,
  title,
  showLabel = false,
  className,
  ...props
}: React.ComponentProps<typeof Button> & {
  icon: LucideIcon;
  label: string;
  title?: string;
  showLabel?: boolean;
}) {
  return (
    <Button variant="ghost" title={title ?? label} aria-label={label} className={cn("h-[30px] gap-[5px] px-2 text-xs", className)} {...props}>
      <Icon />
      {showLabel && <span className="max-lg:hidden">{label}</span>}
    </Button>
  );
}

export function ToolSeparator() {
  return <span className="mx-1 h-5 border-l" />;
}

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

function TreeFolder({ id, name, depth, activeId }: { id: string; name: string; depth: number; activeId?: string }) {
  const open = useExpanded().has(id);
  const children = useQuery({
    queryKey: ["children", id, "folders"],
    queryFn: () => api.children(id, "name", "asc", true),
    enabled: open,
  });
  const empty = children.data?.length === 0;
  return (
    <>
      <NavMenu to={`/files/${id}`} nodeId={id}>
        <div
          className={cn(
            "group flex h-[29px] items-center rounded text-muted-foreground hover:bg-muted",
            activeId === id && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
          )}
          style={{ paddingLeft: depth * 12 }}
        >
          <button
            type="button"
            aria-label={open ? t("Collapse") : t("Expand")}
            className={cn("flex h-full w-5 shrink-0 items-center justify-center", empty && "invisible")}
            onClick={() => setExpanded(id, !open)}
          >
            <ChevronRightIcon className={cn("size-3.5 transition-transform", open && "rotate-90")} />
          </button>
          <Link to={`/files/${id}`} className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2" title={name}>
            {open ? <FolderOpenIcon className="size-[15px] shrink-0" /> : <FolderIcon className="size-[15px] shrink-0" />}
            <span className="truncate">{name}</span>
          </Link>
        </div>
      </NavMenu>
      {open && children.data?.map((c) => <TreeFolder key={c.id} id={c.id} name={c.name} depth={depth + 1} activeId={activeId} />)}
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
  offline,
}: {
  rootId: string;
  to: string;
  icon: LucideIcon;
  label: string;
  activeId?: string;
  depth?: number;
  /** Why the storage service is offline */
  offline?: string | null;
}) {
  const open = useExpanded().has(rootId);
  const folders = useQuery({
    queryKey: ["children", rootId, "folders"],
    queryFn: () => api.children(rootId, "name", "asc", true),
    enabled: open,
  });
  return (
    <>
      <NavMenu to={to} nodeId={rootId} isSpaceRoot>
        <div
          className={cn(
            "flex h-[29px] items-center rounded text-muted-foreground hover:bg-muted",
            activeId === rootId && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
          )}
          style={{ paddingLeft: depth * 12 }}
        >
          <button
            type="button"
            aria-label={open ? t("Collapse") : t("Expand")}
            className={cn("flex h-full w-5 shrink-0 items-center justify-center", folders.data?.length === 0 && "invisible")}
            onClick={() => setExpanded(rootId, !open)}
          >
            <ChevronRightIcon className={cn("size-3.5 transition-transform", open && "rotate-90")} />
          </button>
          <Link
            to={to}
            className="flex h-full min-w-0 flex-1 items-center gap-[7px] pr-2"
            title={offline ? t("{name}: storage service offline ({reason}). You can browse, but you can't open, download, or upload files.", { name: label, reason: tServer(offline) }) : undefined}
          >
            <Icon className={cn("size-[15px] shrink-0", offline && "opacity-40")} />
            <span className="truncate">{label}</span>
            {offline && <CloudOffIcon className="ml-auto size-3.5 shrink-0 text-destructive" aria-label={t("Offline")} />}
          </Link>
        </div>
      </NavMenu>
      {open && folders.data?.map((f) => <TreeFolder key={f.id} id={f.id} name={f.name} depth={depth + 1} activeId={activeId} />)}
    </>
  );
}

/** "All spaces": can be expanded to list every space I can access */
function ThisPc({ activeId }: { activeId?: string }) {
  const open = useExpanded().has("this-pc");
  const drives = useDrives();
  return (
    <>
      <NavMenu to="/drives">
        <div className="flex h-[29px] items-center rounded text-muted-foreground hover:bg-muted">
          <button
            type="button"
            aria-label={open ? t("Collapse") : t("Expand")}
            className="flex h-full w-5 shrink-0 items-center justify-center"
            onClick={() => setExpanded("this-pc", !open)}
          >
            <ChevronRightIcon className={cn("size-3.5 transition-transform", open && "rotate-90")} />
          </button>
          <NavLink
            to="/drives"
            end
            className={({ isActive }) =>
              cn("-ml-5 flex h-full min-w-0 flex-1 items-center gap-[7px] rounded pr-2 pl-5", isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)]")
            }
          >
            <LayersIcon className="size-[15px] shrink-0" />
            <span className="truncate">{t("All spaces")}</span>
          </NavLink>
        </div>
      </NavMenu>
      {open &&
        drives.data?.map((d) => (
          <SpaceRoot
            key={d.id}
            rootId={d.root_id}
            to={`/files/${d.root_id}`}
            icon={DRIVE_ICON[d.kind]}
            label={d.name}
            activeId={activeId}
            depth={1}
            offline={d.offline}
          />
        ))}
    </>
  );
}

// ───────────── Left-hand locations list ─────────────

const NAV_DEFAULT_WIDTH = 200;
const NAV_MIN_WIDTH = 150;
const NAV_MAX_WIDTH = 480;

function NavItem({ to, icon: Icon, label, end }: { to: string; icon: LucideIcon; label: string; end?: boolean }) {
  return (
    <NavMenu to={to}>
      <NavLink
        to={to}
        end={end}
        className={({ isActive }) =>
          cn(
            "flex h-[29px] items-center gap-[7px] rounded px-2 whitespace-nowrap text-muted-foreground hover:bg-muted",
            isActive && "bg-selection text-accent-foreground shadow-[inset_3px_0_0_var(--color-brand)] hover:bg-selection",
          )
        }
      >
        <Icon className="size-[15px] shrink-0" />
        <span className="truncate">{label}</span>
      </NavLink>
    </NavMenu>
  );
}

function LocationsNav({ open, activeFolder, onNavigate }: { open: boolean; activeFolder?: string; onNavigate(): void }) {
  const me = useMe();
  const { dark, mode, canToggle, setMode } = useTheme();
  const [changingPassword, setChangingPassword] = useState(false);
  const [showLogins, setShowLogins] = useState(false);
  const [showAccounts, setShowAccounts] = useState(false);
  const [width, setWidth] = usePersisted("tf-nav-width", NAV_DEFAULT_WIDTH);
  const usedPct = me.quota_bytes > 0 ? Math.min(100, (me.used_bytes / me.quota_bytes) * 100) : 0;
  const usage = me.quota_bytes > 0 ? `${formatBytes(me.used_bytes)} / ${formatBytes(me.quota_bytes)}` : formatBytes(me.used_bytes);
  // On phones the pane opens over the page (with a backdrop): keep focus in it until it closes
  const ref = useRef<HTMLElement>(null);
  const phone = useMediaQuery("(max-width: 47.99rem)");
  useOverlayFocus(ref, open && phone, { onClose: onNavigate });

  const logout = async () => {
    await api.logout().catch(() => {});
    leaveAfterSignOut(me.id);
  };

  return (
    <nav
      ref={ref}
      aria-label={t("File locations")}
      onClick={(e) => (e.target as HTMLElement).closest("a") && onNavigate()}
      style={{ width, maxWidth: "85vw" }}
      className={cn(
        "relative flex shrink-0 flex-col border-r bg-sidebar max-md:hidden",
        open && "max-md:absolute max-md:inset-y-0 max-md:left-0 max-md:z-10 max-md:flex max-md:shadow-xl",
      )}
    >
      <Resizer
        width={width}
        onChange={setWidth}
        min={NAV_MIN_WIDTH}
        max={NAV_MAX_WIDTH}
        defaultWidth={NAV_DEFAULT_WIDTH}
        edge="right"
        label={t("Resize navigation pane")}
      />
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-2.5">
        <NavItem to="/recent" icon={ClockIcon} label={t("Recent")} />
        <NavItem to="/favorites" icon={StarIcon} label={t("Favorite")} />
        <div className="my-2 border-t" />
        <ThisPc activeId={activeFolder} />
        <NavItem to="/shared-with-me" icon={UsersRoundIcon} label={t("Shared with me")} />
        <NavItem to="/shares" icon={Link2Icon} label={t("My share links")} />
        <div className="mt-3 grid gap-0 border-t pt-2">
          <NavItem to="/trash" icon={Trash2Icon} label={t("Trash")} />
        </div>
        {me.role === "admin" && (
          <>
            <div className="px-2 pt-[15px] pb-[5px] text-[11px] text-muted-foreground">{t("Administration")}</div>
            <NavItem to="/admin" icon={SettingsIcon} label={t("Control panel")} />
          </>
        )}
      </div>

      <div className="grid gap-2 border-t p-2">
        {me.quota_bytes > 0 && (
          <div className="grid gap-1 px-1">
            <div
              role="progressbar"
              aria-label={t("Storage used")}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(usedPct)}
              aria-valuetext={usage}
              className="h-1 overflow-hidden rounded-full bg-muted"
            >
              <div className={cn("h-full rounded-full", usedPct > 90 ? "bg-destructive" : "bg-brand")} style={{ width: `${usedPct}%` }} />
            </div>
          </div>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger render={<button type="button" className="flex items-center gap-2 rounded px-1 py-1 text-left hover:bg-muted" />}>
            <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-brand text-[11px] font-medium text-brand-foreground uppercase">
              {me.username.slice(0, 1)}
            </span>
            <span className="min-w-0 flex-1 truncate text-xs" title={me.display_name ? me.username : undefined}>
              {me.display_name || me.username}
            </span>
            <ChevronsUpDownIcon className="size-3.5 text-muted-foreground" />
          </DropdownMenuTrigger>
          <DropdownMenuContent side="top" className="w-52">
            <div className="px-1.5 py-1 text-xs text-muted-foreground">
              {me.role === "admin" ? t("Administrator · {size} used", { size: usage }) : t("User · {size} used", { size: usage })}
            </div>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => setChangingPassword(true)}>
              <KeyRoundIcon /> {t("Change password")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => setShowAccounts(true)}>
              <Link2Icon /> {t("Sign-in methods")}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => setShowLogins(true)}>
              <HistoryIcon /> {t("My sign-in history")}
            </DropdownMenuItem>
            {canToggle && (
              <DropdownMenuSub>
                <DropdownMenuSubTrigger>{dark ? <MoonIcon /> : <SunIcon />} {t("Appearance")}</DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  <DropdownMenuRadioGroup value={mode} onValueChange={(v) => setMode(v as ThemeMode)}>
                    <DropdownMenuRadioItem value="system">{t("Use system setting")}</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="light">{t("Light")}</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="dark">{t("Dark")}</DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            )}
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>
                <LanguagesIcon /> {t("Language")}
                {lang === "zh-TW" && " · Language"}
              </DropdownMenuSubTrigger>
              <DropdownMenuSubContent>
                <DropdownMenuRadioGroup value={lang} onValueChange={(v) => setLang(v as Lang)}>
                  {LANGS.map((l) => (
                    <DropdownMenuRadioItem key={l.id} value={l.id}>
                      {l.label}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuSubContent>
            </DropdownMenuSub>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={logout}>
              <LogOutIcon /> {t("Sign out")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {changingPassword && <ChangePasswordDialog onClose={() => setChangingPassword(false)} />}
      {showAccounts && <LinkedAccountsDialog onClose={() => setShowAccounts(false)} />}
      {showLogins && (
        <LoginLogDialog
          title={t("My sign-in history")}
          description={t("If you see an IP address or device you don't recognize, change your password immediately. You'll be signed out on other devices.")}
          onClose={() => setShowLogins(false)}
        />
      )}
    </nav>
  );
}

// ───────────── Address bar ─────────────

export interface Crumb {
  label: string;
  to?: string;
  /** Virtual levels in the UI (e.g. "All spaces"): shown in the breadcrumb but not included in the copied path (the path doesn't change with the UI language) */
  virtual?: boolean;
}

function SearchInput({ placeholder, onSearch }: { placeholder: string; onSearch?: (q: string) => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const [params] = useSearchParams();
  const onSearchPage = location.pathname === "/search";
  // When the page filters itself (control panel), the search string lives in the URL's ?q=, so typing can continue after jumping over from a settings page
  const initial = onSearchPage || onSearch ? (params.get("q") ?? "") : "";
  const [q, setQ] = useState(initial);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  // Cancel a pending search when leaving the page, so we don't jump to the search page after unmounting
  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    if (!onSearchPage && !onSearch) setQ("");
  }, [onSearchPage, onSearch]);

  const change = (v: string) => {
    setQ(v);
    // The page filters itself (e.g. control panel): don't jump to file search
    if (onSearch) return onSearch(v);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      if (v.trim())
        navigate(`/search?q=${encodeURIComponent(v.trim())}`, {
          replace: onSearchPage,
        });
    }, 350);
  };

  return (
    <div className="relative max-sm:w-full">
      <SearchIcon className="pointer-events-none absolute top-1/2 left-2 size-[13px] -translate-y-1/2 text-muted-foreground" />
      <Input
        value={q}
        onChange={(e) => change(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") change("");
        }}
        placeholder={placeholder}
        aria-label={placeholder}
        autoFocus={!!onSearch && !!initial}
        className="h-[30px] w-[210px] pl-7 text-xs max-lg:w-[160px] max-sm:w-full md:text-xs"
      />
    </div>
  );
}

/** Path for copying: skips virtual levels like "All spaces", e.g. "/ESG Project/Reports" */
export function crumbPath(crumbs: Crumb[]) {
  return "/" + crumbs.filter((c) => !c.virtual).map((c) => c.label).join("/");
}

/** Windows 11 style address bar: click empty space to show the full path, which can be copied directly */
function AddressBar({
  crumbs,
  path,
  upTo,
  searchPlaceholder,
  onSearch,
  icon: Icon,
}: {
  crumbs: Crumb[];
  path: string;
  upTo?: string | null;
  searchPlaceholder: string;
  onSearch?: (q: string) => void;
  icon: LucideIcon;
}) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const tabs = useTabsState();
  const { back, forward } = useTabActions();
  const tab = tabs.tabs.find((t) => t.id === tabs.active);
  const canBack = !!tab && tab.index > 0;
  const canForward = !!tab && tab.index < tab.entries.length - 1;
  const [refreshing, setRefreshing] = useState(false);
  const [editing, setEditing] = useState(false);
  const [copied, setCopied] = useState(false);

  const nav = "size-8 rounded-md [&_svg]:size-[18px]";

  return (
    <div className="flex shrink-0 items-center gap-1 px-2 py-1.5 max-sm:flex-wrap">
      <Button variant="ghost" size="icon" className={nav} aria-label={t("Back")} title={t("Back")} disabled={!canBack} onClick={back}>
        <ArrowLeftIcon />
      </Button>
      <Button variant="ghost" size="icon" className={nav} aria-label={t("Forward")} title={t("Forward")} disabled={!canForward} onClick={forward}>
        <ArrowRightIcon />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={nav}
        aria-label={t("Up")}
        title={t("Up to parent folder")}
        disabled={!upTo}
        onClick={() => upTo && navigate(upTo)}
      >
        <ArrowUpIcon />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={cn(nav, "mr-1")}
        aria-label={t("Refresh")}
        title={t("Refresh")}
        onClick={async () => {
          setRefreshing(true);
          await qc.invalidateQueries();
          setRefreshing(false);
        }}
      >
        <RefreshCwIcon className={cn(refreshing && "animate-spin")} />
      </Button>
      <div
        className="flex h-8 min-w-[140px] flex-1 items-center gap-1 rounded-md bg-muted/70 pl-2.5 focus-within:ring-1 focus-within:ring-ring"
        onClick={(e) => {
          if (!(e.target as HTMLElement).closest("a,button")) setEditing(true);
        }}
      >
        {editing ? (
          <input
            className="h-full min-w-0 flex-1 bg-transparent text-[13px] outline-none select-text"
            aria-label={t("Full path")}
            value={path}
            readOnly
            autoFocus
            onFocus={(e) => e.currentTarget.select()}
            onBlur={() => setEditing(false)}
            onKeyDown={(e) => {
              if (e.key === "Escape" || e.key === "Enter") setEditing(false);
            }}
          />
        ) : (
          <>
            <Icon className="size-4 shrink-0 text-muted-foreground" />
            <nav aria-label={t("File path")} className="flex min-w-0 flex-1 items-center overflow-x-auto text-[13px] whitespace-nowrap">
              {crumbs.map((c, i) => (
                <span key={i} className="flex items-center">
                  <ChevronRightIcon className="mx-0.5 size-3.5 shrink-0 text-muted-foreground" />
                  {c.to && i < crumbs.length - 1 ? (
                    <NavMenu
                      to={c.to}
                      path={crumbPath(crumbs.slice(0, i + 1))}
                    >
                      <Link to={c.to} className="rounded px-1.5 py-1 hover:bg-accent">
                        {c.label}
                      </Link>
                    </NavMenu>
                  ) : (
                    <span className="px-1.5 py-1">{c.label}</span>
                  )}
                </span>
              ))}
            </nav>
          </>
        )}
        <Button
          variant="ghost"
          size="icon-sm"
          className="size-7 text-muted-foreground"
          aria-label={t("Copy path")}
          title={copied ? t("Path copied") : t("Copy path")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={async () => {
            await copyText(path);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </Button>
      </div>
      <SearchInput placeholder={searchPlaceholder} onSearch={onSearch} />
    </div>
  );
}

// ───────────── Frame ─────────────

export interface FrameProps {
  /** Command bar (below the address bar) */
  toolbar: ReactNode;
  crumbs: Crumb[];
  /** Address bar icon */
  icon?: LucideIcon;
  /** Full path used when copying the path */
  path?: string;
  upTo?: string | null;
  searchPlaceholder?: string;
  /** When provided, the search box only filters the current page */
  onSearch?: (q: string) => void;
  activeFolder?: string;
  footer?: ReactNode;
  /** Right side of the status bar (e.g. view switcher) */
  footerRight?: ReactNode;
  children: ReactNode;
}

export const MAIN_ID = "tf-main";

export function Frame(p: FrameProps) {
  const me = useMe();
  const [navOpen, setNavOpen] = useState(false);
  const title = p.crumbs[p.crumbs.length - 1]?.label ?? "";
  const siteName = useBranding().site_name;
  useEffect(() => {
    if (title) setActiveTitle(title);
    document.title = title ? `${title} - ${siteName}` : siteName;
  }, [title, siteName]);
  return (
    <section className="flex h-full min-h-0 flex-col bg-background text-[13px]" aria-label={t("File Explorer")}>
      <h1 className="sr-only">{title}</h1>
      <AddressBar
        crumbs={p.crumbs}
        icon={p.icon ?? FolderIcon}
        path={p.path ?? crumbPath(p.crumbs)}
        upTo={p.upTo}
        searchPlaceholder={p.searchPlaceholder ?? t("Search {name}", { name: title })}
        onSearch={p.onSearch}
      />
      <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-1 border-y px-3 py-1.5 max-lg:px-2">
        <ToolButton icon={PanelLeftIcon} label={t("Location")} showLabel className="md:hidden" aria-expanded={navOpen} onClick={() => setNavOpen(!navOpen)} />
        {p.toolbar}
      </div>
      <div className="relative flex min-h-0 flex-1 overflow-hidden">
        <LocationsNav open={navOpen} activeFolder={p.activeFolder} onNavigate={() => setNavOpen(false)} />
        {navOpen && <div className="absolute inset-0 z-[5] bg-black/20 md:hidden" onClick={() => setNavOpen(false)} />}
        {/* Target of the "Skip to main content" link (AppShell) */}
        <div id={MAIN_ID} tabIndex={-1} className="relative flex min-w-0 flex-1 flex-col outline-none">
          {p.children}
        </div>
      </div>
      <footer className="flex h-7 shrink-0 items-center gap-3 px-3 text-xs text-muted-foreground">
        {p.footer}
        <span className="flex-1" />
        <span className="max-sm:hidden">
          {t("{size} used", { size: me.quota_bytes > 0 ? `${formatBytes(me.used_bytes)} / ${formatBytes(me.quota_bytes)}` : formatBytes(me.used_bytes) })}
        </span>
        {p.footerRight}
      </footer>
    </section>
  );
}

/** When opening a folder, auto-expand its parent folders in the left-hand tree */
export function expandPath(ids: string[]) {
  ids.forEach((id) => setExpanded(id, true));
}
