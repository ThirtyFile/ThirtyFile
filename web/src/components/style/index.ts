/**
 * The style layer: each interface style provides its parts (types.ts), and the explorer asks for the current one's with
 * `useStyleKit()`. A style that isn't offered yet (lib/style `READY_STYLES`) gets the Windows one.
 */
import { createContext, createElement, useContext, useEffect, useSyncExternalStore } from "react";
import type { ViewMode } from "@/components/fileList/layout";
import { useMediaQuery } from "@/lib/focus";
import { usePersisted } from "@/lib/session";
import { useInterfaceStyle, type Style } from "@/lib/style";
import { createStore } from "@/lib/store";
import { isChunkLoadError, reloadForNewVersion } from "@/lib/reload";
import type { StyleKit, ViewChoice } from "./types";
import { loadMacArt } from "./mac/loadArt";
import { windowsKit } from "./windows";

export type { FrameParts, FramePlace, ItemIconProps, OwnViewProps, ShortcutGroup, ShortcutRow, StyleKit, ViewChoice } from "./types";

/**
 * The Mac style's kit, a chunk of its own: the Windows style never loads it. It loads once the Mac style is in use, or
 * with the page when the page was last shown in it. `undefined` while it loads, null when it couldn't (the Windows
 * style's then).
 */
const mac = createStore<StyleKit | null | undefined>(undefined);
let macLoading: Promise<void> | null = null;

/** Starts loading the Mac kit and its look, once; done when the kit is there (or couldn't be) */
export function loadMacKit(): Promise<void> {
  loadMacArt();
  macLoading ??= import("./mac/macKit").then(
    (m) => mac.set(m.macKit),
    (e: unknown) => {
      // The site was updated since the page loaded: the new version has its own
      if (isChunkLoadError(e) && reloadForNewVersion()) return;
      console.error("The Mac style didn't load", e);
      mac.set(null);
    },
  );
  return macLoading;
}

/** Whether the parts of the style in use are there (the Mac style's load on demand): `StyleReady` waits for them */
export function useStyleReady(): boolean {
  const { style } = useInterfaceStyle();
  const loaded = useSyncExternalStore(mac.subscribe, mac.get);
  if (style === "mac") void loadMacKit();
  return style !== "mac" || loaded !== undefined;
}

/** Should something ask for the Mac kit before it has loaded (`StyleReady` keeps the pages from that), a frame that waits
 * rather than the Windows style */
const waitingKit: StyleKit = { ...windowsKit, id: "mac", Frame: () => createElement("div", { "aria-busy": true, className: "flex-1" }) };

/** The kits there are, looked up when asked for (the kits' modules import the explorer, which imports this): the Mac
 * style's once it has loaded */
export function kits(): Partial<Record<Style, StyleKit>> {
  const m = mac.get();
  return m ? { windows: windowsKit, mac: m } : { windows: windowsKit };
}

/** A kit to use instead of the person's style (tests) */
export const StyleKitContext = createContext<StyleKit | null>(null);

/** The parts of the style in use */
export function useStyleKit(): StyleKit {
  const forced = useContext(StyleKitContext);
  const { style } = useInterfaceStyle();
  const phone = useMediaQuery("(max-width: 47.99rem)");
  const loaded = useSyncExternalStore(mac.subscribe, mac.get);
  if (style === "mac" && !forced) void loadMacKit();
  const kit = forced ?? (style === "mac" ? (loaded === undefined ? waitingKit : (loaded ?? windowsKit)) : windowsKit);
  // Phones get the layout every style shares (the Windows style's frame), and with it its menus, keys and ways: a
  // style other than Windows keeps only its look and icons there
  return phone && !forced && kit !== windowsKit ? onPhones(kit) : kit;
}

const phoneKits = new Map<StyleKit, StyleKit>();

/** `kit` as phones have it: the Windows style's parts, with `kit`'s name (its look applies) and icons */
function onPhones(kit: StyleKit): StyleKit {
  let k = phoneKits.get(kit);
  if (!k) {
    k = { ...windowsKit, id: kit.id, ItemIcon: kit.ItemIcon };
    phoneKits.set(kit, k);
  }
  return k;
}

/** The style the page was last shown in (kept in the browser), whose assets start loading with the page */
const SHOWN_KEY = "tf-style-shown";
try {
  if (localStorage.getItem(SHOWN_KEY) === "mac") void loadMacKit();
} catch {
  // Not in every environment the modules load in (tests), or storage is forbidden
}

/**
 * Puts the style in use on the page's root (`data-style`), where its look's tokens apply (style.css; the Mac style's in
 * mac/art/mac.css). Used by the pages that show a style's parts: the frame of signed-in pages, and share links.
 */
export function useApplyStyle() {
  const { id } = useStyleKit();
  useEffect(() => {
    document.documentElement.dataset.style = id;
    try {
      localStorage.setItem(SHOWN_KEY, id);
    } catch {
      // Ignore when the browser forbids storage
    }
  }, [id]);
}

/** The views of the file list the style offers on this screen (phones have fewer) */
export function useViews(): readonly ViewChoice[] {
  const kit = useStyleKit();
  const phone = useMediaQuery("(max-width: 47.99rem)");
  const views = kit.views();
  return phone ? views.filter((v) => !v.notOnPhones) : views;
}

/** The view of the file list: the one chosen (kept in the browser), when it is offered here, else the style's own */
export function useView(): [ViewMode, (view: ViewMode) => void] {
  const kit = useStyleKit();
  const views = useViews();
  const [kept, setView] = usePersisted<ViewMode>("tf-view", kit.defaultView);
  return [views.some((v) => v.id === kept) ? kept : kit.defaultView, setView];
}
