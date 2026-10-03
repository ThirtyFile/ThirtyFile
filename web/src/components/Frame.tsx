import { useEffect, useState, type ReactNode } from "react";
import { FolderIcon, type LucideIcon } from "lucide-react";
import { ShortcutsHost } from "@/components/ShortcutsDialog";
import { useStyleKit } from "@/components/style";
import { useDrives } from "@/lib/drives";
import { useBranding } from "@/lib/branding";
import { t } from "@/lib/i18n";
import { formatBytes } from "@/lib/utils";
import { setActiveTitle } from "@/tabs";
import { AddressBar, crumbPath, type Crumb } from "./frame/AddressBar";
import { LocationsNav } from "./frame/LocationsNav";

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

export { MAIN_ID } from "@/components/style/types";

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
  // The parts, which the style places (components/style)
  const Layout = useStyleKit().Frame;
  return (
    <main className="flex h-full min-h-0 flex-col bg-background text-[13px]" aria-label={t("File Explorer")}>
      <h1 className="sr-only">{title}</h1>
      <Layout
        pathBar={
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
        }
        toolbar={p.toolbar}
        nav={<LocationsNav open={navOpen} activeFolder={p.activeFolder} onNavigate={() => setNavOpen(false)} />}
        navOpen={navOpen}
        setNavOpen={setNavOpen}
        content={p.children}
        status={p.footer}
        used={
          // How much of the space being browsed is used ("My files" shows its own in the navigation pane)
          space && (
            <span className="max-sm:hidden" title={space.name}>
              {t("{size} used", { size: space.quota_bytes > 0 ? `${formatBytes(space.used_bytes)} / ${formatBytes(space.quota_bytes)}` : formatBytes(space.used_bytes) })}
            </span>
          )
        }
        statusEnd={p.footerRight}
      />
      {p.keys && <ShortcutsHost />}
    </main>
  );
}
