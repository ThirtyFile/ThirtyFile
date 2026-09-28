import { useRef } from "react";
import { NavLink } from "react-router";
import { ClockIcon, Link2Icon, SettingsIcon, StarIcon, Trash2Icon, UsersRoundIcon, type LucideIcon } from "lucide-react";
import { NavMenu } from "@/components/NavMenu";
import { Resizer } from "@/components/Resizer";
import { FolderTree, FolderTreeToolbar } from "@/components/FolderTree";
import { useMediaQuery, useOverlayFocus } from "@/lib/focus";
import { hasPersonal } from "@/lib/home";
import { usePersisted, useMe } from "@/lib/session";
import { t } from "@/lib/i18n";
import { cn, formatBytes } from "@/lib/utils";
import { AccountMenu } from "./AccountMenu";

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

export function LocationsNav({ open, activeFolder, onNavigate }: { open: boolean; activeFolder?: string; onNavigate(): void }) {
  const me = useMe();
  const [width, setWidth] = usePersisted("tf-nav-width", NAV_DEFAULT_WIDTH);
  // The quota and usage are those of "My files": nothing to show for someone without it
  const personal = hasPersonal(me);
  const usedPct = me.quota_bytes > 0 ? Math.min(100, (me.used_bytes / me.quota_bytes) * 100) : 0;
  const usage = !personal ? null : me.quota_bytes > 0 ? `${formatBytes(me.used_bytes)} / ${formatBytes(me.quota_bytes)}` : formatBytes(me.used_bytes);
  // On phones the pane opens over the page (with a backdrop): keep focus in it until it closes
  const ref = useRef<HTMLElement>(null);
  const phone = useMediaQuery("(max-width: 47.99rem)");
  useOverlayFocus(ref, open && phone, { onClose: onNavigate });

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
      <FolderTreeToolbar activeId={activeFolder} />
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-2.5">
        <NavItem to="/recent" icon={ClockIcon} label={t("Recent")} />
        <NavItem to="/favorites" icon={StarIcon} label={t("Favorites")} />
        <div className="my-2 border-t" />
        <FolderTree activeId={activeFolder} />
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
        {usage !== null && me.quota_bytes > 0 && (
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
        <AccountMenu usage={usage} />
      </div>
    </nav>
  );
}
