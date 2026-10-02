import { useEffect, useState, type ReactNode } from "react";
import { FolderIcon, PanelLeftIcon, type LucideIcon } from "lucide-react";
import { ShortcutsHost } from "@/components/ShortcutsDialog";
import { useDrives } from "@/lib/drives";
import { useBranding } from "@/lib/branding";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { setActiveTitle } from "@/tabs";
import { AddressBar, crumbPath, type Crumb } from "./frame/AddressBar";
import { LocationsNav } from "./frame/LocationsNav";
import { ToolButton } from "./frame/ToolButton";

export { crumbPath, type Crumb };
export { ToolButton, ToolSeparator } from "./frame/ToolButton";

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
  /** The space being browsed: the status bar shows how much of it is used */
  space?: string;
  footer?: ReactNode;
  /** Right side of the status bar (e.g. view switcher) */
  footerRight?: ReactNode;
  /** File explorer keys: Alt+arrows and Backspace to move around, Ctrl+F, Ctrl+L, F5, "?" for the list of shortcuts */
  keys?: boolean;
  children: ReactNode;
}

export const MAIN_ID = "tf-main";

export function Frame(p: FrameProps) {
  const drives = useDrives();
  const space = p.space ? drives.data?.find((d) => d.id === p.space) : undefined;
  const [navOpen, setNavOpen] = useState(false);
  const title = p.crumbs[p.crumbs.length - 1]?.label ?? "";
  const siteName = useBranding().site_name;
  useEffect(() => {
    if (title) setActiveTitle(title);
    document.title = title ? `${title} - ${siteName}` : siteName;
  }, [title, siteName]);
  return (
    <main className="flex h-full min-h-0 flex-col bg-background text-[13px]" aria-label={t("File Explorer")}>
      <h1 className="sr-only">{title}</h1>
      <AddressBar
        crumbs={p.crumbs}
        icon={p.icon ?? FolderIcon}
        path={p.path ?? crumbPath(p.crumbs)}
        upTo={p.upTo}
        searchPlaceholder={p.searchPlaceholder ?? (p.activeFolder ? t("Search {name}", { name: title }) : t("Search all spaces"))}
        onSearch={p.onSearch}
        searchIn={p.activeFolder}
        keys={p.keys}
      />
      <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-1 border-y px-3 py-1.5 max-lg:px-2">
        <ToolButton icon={PanelLeftIcon} label={t("Navigation pane")} showLabel className="md:hidden" aria-expanded={navOpen} onClick={() => setNavOpen(!navOpen)} />
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
        {/* How much of the space being browsed is used ("My files" shows its own in the navigation pane) */}
        {space && (
          <span className="max-sm:hidden" title={space.name}>
            {t("{size} used", { size: space.quota_bytes > 0 ? `${formatBytes(space.used_bytes)} / ${formatBytes(space.quota_bytes)}` : formatBytes(space.used_bytes) })}
          </span>
        )}
        {p.footerRight}
      </footer>
      {p.keys && <ShortcutsHost />}
    </main>
  );
}
