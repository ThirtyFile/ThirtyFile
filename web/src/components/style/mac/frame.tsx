/**
 * The Mac style's frame, as a Finder window's: the sidebar down the left; on the right, the toolbar (back and forward,
 * the place's name, the page's commands and the search box), the page, the path bar and the status bar. Phones get
 * the layout every style shares (the Windows style's frame).
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";
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
import { CompactToolbar, useMacWindowPrefs, useSingleMacTab } from "./windowPrefs";
import { NotificationBell } from "@/components/NotificationBell";
import { ErrorBoundary } from "@/components/ErrorBoundary";

export function MacFrame(f: FrameParts) {
  const phone = useMediaQuery("(max-width: 47.99rem)");
  // The style's look, icons and symbols load with it (./loadArt.ts): until they are there, the window waits a moment,
  // then shows with the shared ones (they take their place once they arrive) rather than stay empty on a slow network
  const art = useMacArt();
  const [waited, setWaited] = useState(false);
  useEffect(() => {
    if (art !== undefined) return;
    const timer = setTimeout(() => setWaited(true), ART_WAIT_MS);
    return () => clearTimeout(timer);
  }, [art]);
  if (art === undefined && !waited) return <div aria-busy className="flex-1" />;
  if (phone) return <WindowsFrame {...f} />;
  return <MacWindow {...f} />;
}

/** How long the window waits for the style's assets before showing without them */
const ART_WAIT_MS = 1500;

function MacWindow(f: FrameParts) {
  const { place } = f;
  const k = useStyleKit().keys;
  const { back, forward, canBack, canForward } = useHistory();
  const { refresh } = useRefresh();
  const searchRef = useRef<HTMLInputElement>(null);
  const sym = useMacSymbols();
  const [bars] = useMacWindowPrefs();
  const singleTab = useSingleMacTab();
  const toolbar = useRef<HTMLDivElement>(null);
  const [compact, setCompact] = useState(false);
  useLayoutEffect(() => {
    const el = toolbar.current;
    if (!el) return;
    // Leave room for the view capsule, optional commands, search, notifications and a short title.
    const measure = () => setCompact(el.clientWidth < 820);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
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
        <div
          ref={toolbar}
          data-mac-toolbar
          data-compact={compact || undefined}
          className="tf-mac-toolbar flex h-(--mac-toolbar-h) shrink-0 items-center gap-2 border-b bg-(--mac-toolbar-bg) px-3"
        >
          <div role="group" aria-label={t("Navigation")} className="tf-mac-capsule flex shrink-0 items-center p-0.5">
            <ToolButton icon={sym.back} label={t("Back")} title={`${t("Back")} (${shortcut(k.back[0])})`} className={nav} disabled={!canBack} onClick={back} />
            <ToolButton icon={sym.forward} label={t("Forward")} title={`${t("Forward")} (${shortcut(k.forward[0])})`} className={nav} disabled={!canForward} onClick={forward} />
          </div>
          {/* The page's heading is the frame's (read by screen readers): this is the same name, shown */}
          <div aria-hidden className="min-w-0 flex-1 truncate text-[13px] font-semibold" title={title}>
            {title}
          </div>
          <CompactToolbar value={compact}>{f.toolbar}</CompactToolbar>
          <SearchInput className="tf-mac-search min-w-24 shrink-0" placeholder={place.searchPlaceholder} onSearch={place.onSearch} within={place.searchIn} inputRef={searchRef} />
          {singleTab && (
            <ErrorBoundary>
              <NotificationBell />
            </ErrorBoundary>
          )}
        </div>
        {/* Target of the "Skip to main content" link (AppShell) */}
        <div id={MAIN_ID} tabIndex={-1} className="relative flex min-h-0 min-w-0 flex-1 flex-col outline-none">
          {f.content}
        </div>
        {bars.path && <PathBar place={place} />}
        {bars.status && (
          <footer data-mac-status className="flex h-(--mac-bar-h) shrink-0 items-center gap-3 border-t px-3 text-xs text-muted-foreground">
            {f.status}
            <span className="flex-1" />
            {f.used}
            {f.statusEnd}
          </footer>
        )}
      </div>
      <GoToFolder path={place.path} />
    </div>
  );
}
