/**
 * The Windows style's frame, as File Explorer's: the address bar on top, the command bar under it, the locations on the
 * left of the page, and the status bar at the bottom
 */
import { PanelLeftIcon } from "lucide-react";
import { MAIN_ID, type FrameParts } from "../types";
import { AddressBar } from "@/components/frame/AddressBar";
import { LocationsNav } from "@/components/frame/LocationsNav";
import { ToolButton } from "@/components/frame/ToolButton";
import { t } from "@/lib/i18n";

export function WindowsFrame(f: FrameParts) {
  const { place } = f;
  return (
    <>
      <AddressBar
        crumbs={place.crumbs}
        icon={place.icon}
        path={place.path}
        upTo={place.upTo}
        searchPlaceholder={place.searchPlaceholder}
        onSearch={place.onSearch}
        searchIn={place.searchIn}
        keys={place.keys}
      />
      <div className="flex min-h-12 shrink-0 flex-wrap items-center gap-1 border-y px-3 py-1.5 max-lg:px-2">
        <ToolButton icon={PanelLeftIcon} label={t("Navigation pane")} showLabel className="md:hidden" aria-expanded={f.navOpen} onClick={() => f.setNavOpen(!f.navOpen)} />
        {f.toolbar}
      </div>
      <div className="relative flex min-h-0 flex-1 overflow-hidden">
        <LocationsNav open={f.navOpen} activeFolder={place.activeFolder} onNavigate={() => f.setNavOpen(false)} />
        {f.navOpen && <div className="absolute inset-0 z-[5] bg-black/20 md:hidden" onClick={() => f.setNavOpen(false)} />}
        {/* Target of the "Skip to main content" link (AppShell) */}
        <div id={MAIN_ID} tabIndex={-1} className="relative flex min-w-0 flex-1 flex-col outline-none">
          {f.content}
        </div>
      </div>
      <footer className="flex h-7 shrink-0 items-center gap-3 px-3 text-xs text-muted-foreground">
        {f.status}
        <span className="flex-1" />
        {f.used}
        {f.statusEnd}
      </footer>
    </>
  );
}
