import { useSyncExternalStore } from "react";

/** Appearance: follow system, light, dark */
export type ThemeMode = "system" | "light" | "dark";

const listeners = new Set<() => void>();
const media = matchMedia("(prefers-color-scheme: dark)");

// Default appearance set by the admin in branding settings, and whether users may switch it themselves (injected by the home page HTML first)
let policy: { mode: ThemeMode; allowToggle: boolean } = {
  mode: window.__TF_BRANDING__?.default_mode ?? "system",
  allowToggle: window.__TF_BRANDING__?.allow_toggle ?? true,
};

function stored(): ThemeMode | null {
  try {
    const t = localStorage.getItem("tf-theme");
    return t === "light" || t === "dark" || t === "system" ? t : null;
  } catch {
    return null;
  }
}

/** Appearance currently in use (always the admin's default when switching isn't allowed) */
function currentMode(): ThemeMode {
  return (policy.allowToggle && stored()) || policy.mode;
}

function apply() {
  const mode = currentMode();
  document.documentElement.classList.toggle("dark", mode === "dark" || (mode === "system" && media.matches));
  listeners.forEach((l) => l());
}

media.addEventListener("change", apply);
// The inline script in index.html applies the theme before the page is drawn; when it didn't run (blocked by the
// browser, say), "Follow system" or a chosen appearance would otherwise not show until something changed
apply();

/** Called when branding settings change */
export function setThemePolicy(mode: ThemeMode, allowToggle: boolean) {
  if (policy.mode === mode && policy.allowToggle === allowToggle) return;
  policy = { mode, allowToggle };
  apply();
}

/** User picks an appearance; "system" means follow the system */
export function setThemeMode(mode: ThemeMode) {
  if (!policy.allowToggle) return;
  try {
    localStorage.setItem("tf-theme", mode);
  } catch {
    // Ignore when the browser forbids storage
  }
  apply();
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

export function useTheme() {
  const dark = useSyncExternalStore(subscribe, () => document.documentElement.classList.contains("dark"));
  const mode = useSyncExternalStore(subscribe, currentMode);
  const canToggle = useSyncExternalStore(subscribe, () => policy.allowToggle);
  return { dark, mode, canToggle, setMode: setThemeMode };
}
