/** Error message when a dynamically loaded code chunk can't be found (the site was updated) */
export function isChunkLoadError(e: unknown) {
  const msg = e instanceof Error ? e.message : String(e);
  return /dynamically imported module|Importing a module script failed|error loading dynamically imported module|Unable to preload CSS/i.test(msg);
}

const KEY = "tf-version-reload";

/** Reload once to load the new version; don't retry if already reloaded within 30 seconds (avoids endless reloading); returns whether a reload happened */
export function reloadForNewVersion(): boolean {
  try {
    const last = Number(sessionStorage.getItem(KEY) || 0);
    if (Date.now() - last < 30_000) return false;
    sessionStorage.setItem(KEY, String(Date.now()));
  } catch {
    return false;
  }
  window.location.reload();
  return true;
}
