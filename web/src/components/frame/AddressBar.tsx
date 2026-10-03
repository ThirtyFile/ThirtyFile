import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useSearchParams } from "react-router";
import { ArrowLeftIcon, ArrowRightIcon, ArrowUpIcon, CheckIcon, ChevronRightIcon, CopyIcon, RefreshCwIcon, SearchIcon, type LucideIcon } from "lucide-react";
import { api, ApiError } from "@/api";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NavMenu } from "@/components/NavMenu";
import { useFolderDrop } from "@/lib/dnd";
import { folderOfPath, hasPersonal } from "@/lib/home";
import { liveSearch, type LiveSearch } from "@/lib/liveSearch";
import { useMe } from "@/lib/session";
import { appLink, pathAliases, urlOf } from "@/lib/paths";
import { shortcut } from "@/lib/keys";
import { useStyleKit } from "@/components/style";
import { t } from "@/lib/i18n";
import { cn, copyText } from "@/lib/utils";
import { useTabActions } from "@/tabs";
import { useFrameKeys, useHistory, useRefresh } from "./frameKeys";

// ───────────── Address bar ─────────────

export interface Crumb {
  label: string;
  to?: string;
  /** Virtual levels in the UI (e.g. "All spaces"): shown in the breadcrumb but not included in the copied path (the path doesn't change with the UI language) */
  virtual?: boolean;
}

export function SearchInput({ placeholder, onSearch, within, inputRef }: { placeholder: string; onSearch?: (q: string) => void; within?: string; inputRef?: React.Ref<HTMLInputElement> }) {
  const navigate = useNavigate();
  const location = useLocation();
  const [params] = useSearchParams();
  const onSearchPage = location.pathname === "/search";
  // Searching from a folder looks in that folder and below (the search page keeps the scope it was opened with)
  const scope = onSearchPage ? (params.get("in") ?? undefined) : within;
  // When the page filters itself (control panel), the search string lives in the URL's ?q=, so typing can continue after jumping over from a settings page
  const initial = onSearchPage || onSearch ? (params.get("q") ?? "") : "";
  const [q, setQ] = useState(initial);

  useEffect(() => {
    if (!onSearchPage && !onSearch) setQ("");
  }, [onSearchPage, onSearch]);

  const search = (v: string) => {
    // The page filters itself (e.g. control panel): don't jump to file search
    if (onSearch) return onSearch(v);
    if (v.trim())
      navigate(`/search?q=${encodeURIComponent(v.trim())}${scope ? `&in=${encodeURIComponent(scope)}` : ""}`, {
        replace: onSearchPage,
      });
  };
  const latest = useRef({ search, filters: !!onSearch });
  latest.current = { search, filters: !!onSearch };
  // A page filtering itself does so at once; file search waits for a pause in typing. Neither runs on text an input
  // method is still composing
  const live = useRef<LiveSearch>(null);
  live.current ??= liveSearch(
    (v) => latest.current.search(v),
    () => (latest.current.filters ? 0 : 350),
  );
  // Cancel a pending search when leaving the page, so we don't jump to the search page after unmounting
  useEffect(() => () => live.current?.cancel(), []);

  const change = (v: string, composing = false) => {
    setQ(v);
    live.current!.input(v, composing);
  };

  return (
    <div className="relative max-sm:w-full">
      <SearchIcon className="pointer-events-none absolute top-1/2 left-2 size-[13px] -translate-y-1/2 text-muted-foreground" />
      <Input
        ref={inputRef}
        value={q}
        onChange={(e) => change(e.target.value, (e.nativeEvent as InputEvent).isComposing)}
        onCompositionStart={() => live.current!.compositionStart()}
        onCompositionEnd={(e) => live.current!.compositionEnd(e.currentTarget.value)}
        onKeyDown={(e) => {
          // Enter and Escape choose or drop an input method's candidate while it is composing
          if (e.nativeEvent.isComposing || e.keyCode === 229 || live.current!.composing()) return;
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
  return (
    "/" +
    crumbs
      .filter((c) => !c.virtual)
      .map((c) => c.label)
      .join("/")
  );
}

/** A part of the address bar path: a link to that folder, which also takes dropped items and files */
export function CrumbItem({ crumb: c, last, path }: { crumb: Crumb; last: boolean; path: string }) {
  const folder = folderOfPath(c.to, hasPersonal(useMe()));
  const { dropping, dropProps } = useFolderDrop(folder ? { id: folder, name: c.label } : null);
  const drop = cn(dropping && "bg-brand/15 ring-1 ring-brand ring-inset");
  return (
    <span className="flex items-center">
      <ChevronRightIcon className="mx-0.5 size-3.5 shrink-0 text-muted-foreground" />
      {c.to && !last ? (
        <NavMenu to={c.to} path={path}>
          <Link to={c.to} {...dropProps} className={cn("rounded px-1.5 py-1 hover:bg-accent", drop)}>
            {c.label}
          </Link>
        </NavMenu>
      ) : (
        <span {...dropProps} aria-current={last ? "page" : undefined} className={cn("rounded px-1.5 py-1", drop)}>
          {c.label}
        </span>
      )}
    </span>
  );
}

/**
 * Going to a path typed or pasted (see lib/paths.ts), or to a link to a page of this site: the Windows style's address
 * bar and the Mac style's "Go to folder". `go` resolves to whether it went (when it didn't, the reason was shown).
 */
export function useGoToPath() {
  const navigate = useNavigate();
  const { openFile } = useTabActions();
  const [finding, setFinding] = useState(false);
  const go = async (typed: string): Promise<boolean> => {
    const link = appLink(typed, window.location.origin);
    if (link) {
      void navigate(link);
      return true;
    }
    setFinding(true);
    try {
      const found = await api.findPath(typed, pathAliases());
      if (found.place === "file") openFile(urlOf(found));
      else void navigate(urlOf(found));
      return true;
    } catch (e) {
      toast.error(
        e instanceof ApiError && e.status === 404
          ? t('Can\'t find "{path}". Check the spelling and try again.', { path: typed })
          : e instanceof Error
            ? e.message
            : t("Couldn't open this path"),
      );
      return false;
    } finally {
      setFinding(false);
    }
  };
  return { finding, go };
}

/**
 * The address bar being typed in: shows the full path to copy, and goes to a path typed or pasted into it, or to a
 * link to a page of this site. Escape or leaving the box puts the path back
 */
function PathInput({ path, onDone }: { path: string; onDone(): void }) {
  const [text, setText] = useState(path);
  const { finding, go: goTo } = useGoToPath();
  const ref = useRef<HTMLInputElement>(null);

  const go = async () => {
    const typed = text.trim();
    if (!typed || typed === path) return onDone();
    if (await goTo(typed)) onDone();
    else {
      ref.current?.focus();
      ref.current?.select();
    }
  };

  return (
    <input
      ref={ref}
      className="h-full min-w-0 flex-1 bg-transparent text-[13px] outline-none select-text"
      aria-label={t("Full path")}
      title={t("Type or paste a path, then press Enter")}
      value={text}
      onChange={(e) => setText(e.target.value)}
      readOnly={finding}
      autoFocus
      spellCheck={false}
      autoComplete="off"
      onFocus={(e) => e.currentTarget.select()}
      onBlur={() => !finding && onDone()}
      onKeyDown={(e) => {
        if (e.key === "Escape") onDone();
        else if (e.key === "Enter") void go();
        else return;
        e.preventDefault();
      }}
    />
  );
}

/** Windows 11 style address bar: click empty space to show the full path, which can be copied, or type or paste another path to go there */
export function AddressBar({
  crumbs,
  path,
  upTo,
  searchPlaceholder,
  onSearch,
  searchIn,
  icon: Icon,
  keys,
}: {
  crumbs: Crumb[];
  path: string;
  upTo?: string | null;
  searchPlaceholder: string;
  onSearch?: (q: string) => void;
  /** Folder the search box searches in (and below); none = everything */
  searchIn?: string;
  icon: LucideIcon;
  keys?: boolean;
}) {
  const navigate = useNavigate();
  const { back, forward, canBack, canForward } = useHistory();
  const { refreshing, refresh } = useRefresh();
  const [editing, setEditing] = useState(false);
  const [copied, setCopied] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const trail = useRef<HTMLElement>(null);
  /** The style's keys (components/style) */
  const k = useStyleKit().keys;

  // A long path doesn't fit on a phone: show its end, the folder you're in (like File Explorer), scrolling back for the rest
  const trailKey = crumbs.map((c) => c.label).join("/");
  /** The path is wider than its place: the keyboard can reach it to scroll it (arrows) */
  const [trailScrolls, setTrailScrolls] = useState(false);
  useLayoutEffect(() => {
    const el = trail.current;
    if (!el) return;
    const toEnd = () => {
      el.scrollLeft = el.scrollWidth;
      setTrailScrolls(el.scrollWidth > el.clientWidth);
    };
    toEnd();
    const ro = new ResizeObserver(toEnd);
    ro.observe(el);
    return () => ro.disconnect();
  }, [trailKey, editing]);

  // Moving around with the keyboard, with the style's keys (see the shortcuts dialog)
  useFrameKeys({
    enabled: keys,
    upTo,
    refresh: () => void refresh(),
    focusSearch: () => {
      searchRef.current?.focus();
      searchRef.current?.select();
    },
    editPath: () => setEditing(true),
  });

  const nav = "size-8 rounded-md [&_svg]:size-[18px]";

  return (
    <div className="flex shrink-0 items-center gap-1 px-2 py-1.5 max-sm:flex-wrap">
      <Button variant="ghost" size="icon" className={nav} aria-label={t("Back")} title={`${t("Back")} (${shortcut(k.back[0])})`} disabled={!canBack} onClick={back}>
        <ArrowLeftIcon />
      </Button>
      <Button variant="ghost" size="icon" className={nav} aria-label={t("Forward")} title={`${t("Forward")} (${shortcut(k.forward[0])})`} disabled={!canForward} onClick={forward}>
        <ArrowRightIcon />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className={nav}
        aria-label={t("Up")}
        title={`${t("Up to parent folder")} (${shortcut(k.upFolder[0])})`}
        disabled={!upTo}
        onClick={() => upTo && navigate(upTo)}
      >
        <ArrowUpIcon />
      </Button>
      <Button variant="ghost" size="icon" className={cn(nav, "mr-1")} aria-label={t("Refresh")} title={`${t("Refresh")} (${shortcut(k.refresh[0])})`} onClick={refresh}>
        <RefreshCwIcon className={cn(refreshing && "animate-spin")} />
      </Button>
      <div
        className="flex h-8 min-w-[140px] flex-1 items-center gap-1 rounded-md bg-muted/70 pl-2.5 focus-within:ring-1 focus-within:ring-ring"
        onClick={(e) => {
          if (!(e.target as HTMLElement).closest("a,button")) setEditing(true);
        }}
      >
        {editing ? (
          <PathInput path={path} onDone={() => setEditing(false)} />
        ) : (
          <>
            <Icon className="size-4 shrink-0 text-muted-foreground" />
            <nav
              ref={trail}
              aria-label={t("File path")}
              tabIndex={trailScrolls ? 0 : undefined}
              // Scrolls sideways (to the end at first) without a scroll bar inside the address bar
              className="flex min-w-0 flex-1 items-center overflow-x-auto rounded-sm text-[13px] whitespace-nowrap outline-none [scrollbar-width:none] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset [&::-webkit-scrollbar]:hidden"
            >
              {crumbs.map((c, i) => (
                <CrumbItem key={i} crumb={c} last={i === crumbs.length - 1} path={crumbPath(crumbs.slice(0, i + 1))} />
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
            if (!(await copyText(path))) return void toast.error(t("Couldn't copy"));
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? <CheckIcon /> : <CopyIcon />}
        </Button>
        {/* Screen readers hear that the path was copied */}
        <span role="status" className="sr-only">
          {copied ? t("Path copied") : ""}
        </span>
      </div>
      <SearchInput placeholder={searchPlaceholder} onSearch={onSearch} within={searchIn} inputRef={searchRef} />
    </div>
  );
}
