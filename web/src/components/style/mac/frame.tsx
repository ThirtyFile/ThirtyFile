/**
 * The Mac style's frame, as a Finder window's: the sidebar down the left; on the right, the toolbar (back and forward,
 * the place's name, the page's commands and the search box), the page, the path bar and the status bar. Phones get
 * the layout every style shares (the Windows style's frame).
 */
import { useRef } from "react";
import { MAIN_ID, type FrameParts } from "../types";
import { WindowsFrame } from "../windows/frame";
import { SearchInput } from "@/components/frame/AddressBar";
import { useFrameKeys, useHistory, useRefresh } from "@/components/frame/frameKeys";
import { ToolButton } from "@/components/frame/ToolButton";
import { useStyleKit } from "@/components/style";
import { useMediaQuery } from "@/lib/focus";
import { t } from "@/lib/i18n";
import { shortcut } from "@/lib/keys";
import { useMacArt } from "./loadArt";
import { useMacSymbols } from "./look";
import { GoToFolder, PathBar, openGoToFolder } from "./pathBar";
import { MacSidebar } from "./sidebar";

export function MacFrame(f: FrameParts) {
  const phone = useMediaQuery("(max-width: 47.99rem)");
  // The style's look, icons and symbols load with it (./loadArt.ts): until they are there, the window waits
  const art = useMacArt();
  if (art === undefined) return <div aria-busy className="flex-1" />;
  if (phone) return <WindowsFrame {...f} />;
  return <MacWindow {...f} />;
}

function MacWindow(f: FrameParts) {
  const { place } = f;
  const k = useStyleKit().keys;
  const { back, forward, canBack, canForward } = useHistory();
  const { refresh } = useRefresh();
  const searchRef = useRef<HTMLInputElement>(null);
  const sym = useMacSymbols();
  useFrameKeys({
    enabled: place.keys,
    upTo: place.upTo,
    refresh: () => void refresh(),
    focusSearch: () => {
      searchRef.current?.focus();
      searchRef.current?.select();
    },
    editPath: openGoToFolder,
  });
  const title = place.crumbs.at(-1)?.label ?? "";
  const nav = "w-(--tf-tool-h) px-0";
  return (
    // The sidebar is translucent over the window's backdrop (art/mac.css)
    <div className="tf-mac-desktop flex min-h-0 flex-1 overflow-hidden">
      <MacSidebar activeFolder={place.activeFolder} />
      <div className="flex min-w-0 flex-1 flex-col bg-background">
        <div className="flex min-h-(--mac-toolbar-h) shrink-0 flex-wrap items-center gap-1 border-b bg-(--mac-toolbar-bg) px-2 py-1.5">
          <ToolButton icon={sym.back} label={t("Back")} title={`${t("Back")} (${shortcut(k.back[0])})`} className={nav} disabled={!canBack} onClick={back} />
          <ToolButton icon={sym.forward} label={t("Forward")} title={`${t("Forward")} (${shortcut(k.forward[0])})`} className={nav} disabled={!canForward} onClick={forward} />
          {/* The page's heading is the frame's (read by screen readers): this is the same name, shown */}
          <div aria-hidden className="mx-1.5 min-w-0 shrink truncate text-[13px] font-semibold" title={title}>
            {title}
          </div>
          <span className="flex-1" />
          {f.toolbar}
          <SearchInput placeholder={place.searchPlaceholder} onSearch={place.onSearch} within={place.searchIn} inputRef={searchRef} />
        </div>
        {/* Target of the "Skip to main content" link (AppShell) */}
        <div id={MAIN_ID} tabIndex={-1} className="relative flex min-h-0 min-w-0 flex-1 flex-col outline-none">
          {f.content}
        </div>
        <PathBar place={place} />
        <footer className="flex h-(--mac-bar-h) shrink-0 items-center gap-3 border-t px-3 text-xs text-muted-foreground">
          {f.status}
          <span className="flex-1" />
          {f.used}
          {f.statusEnd}
        </footer>
      </div>
      <GoToFolder path={place.path} />
    </div>
  );
}
