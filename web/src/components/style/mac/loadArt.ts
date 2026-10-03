/**
 * Loading the Mac style's own assets (./art: its look, icons and symbols), which are a chunk of their own: the Windows
 * style never loads them. They load once the Mac style is in use, or at once when the page was last shown in it
 * (components/style). Until they are there, the Mac style's frame waits (./frame.tsx); if they can't load, it shows
 * with the shared icons.
 */
import { useSyncExternalStore } from "react";
import { createStore } from "@/lib/store";
import { isChunkLoadError, reloadForNewVersion } from "@/lib/reload";

export type MacArt = typeof import("./art");

/** The assets: `undefined` until they are loaded, null when they couldn't be */
const art = createStore<MacArt | null | undefined>(undefined);
let started = false;

/** Starts loading the assets, once */
export function loadMacArt() {
  if (started) return;
  started = true;
  import("./art").then(
    (loaded) => art.set(loaded),
    (e: unknown) => {
      // The site was updated since the page loaded: the new version has its own
      if (isChunkLoadError(e) && reloadForNewVersion()) return;
      console.error("The Mac style's look didn't load", e);
      art.set(null);
    },
  );
}

/** The assets: `undefined` while they load, null when they couldn't */
export const macArt = art.get;

/** The assets, loading them when not yet asked for: `undefined` while they load, null when they couldn't */
export function useMacArt(): MacArt | null | undefined {
  loadMacArt();
  return useSyncExternalStore(art.subscribe, art.get);
}
