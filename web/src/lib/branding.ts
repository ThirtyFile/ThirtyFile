import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { setThemePolicy, type ThemeMode } from "@/lib/theme";

/** Branding settings (public; also used by the login and share pages) */
export interface Branding {
  site_name: string;
  show_name: boolean;
  light_brand: string;
  dark_brand: string;
  default_mode: ThemeMode;
  allow_toggle: boolean;
  login_title: string;
  login_subtitle: string;
  login_footer: string;
  /** Show a lock screen (clock and date) first on the login page */
  login_lock: boolean;
  has_login_background: boolean;
  has_logo: boolean;
  has_logo_dark: boolean;
  version: number;
}

declare global {
  interface Window {
    /** Branding settings injected by the server into the home page HTML */
    __TF_BRANDING__?: Branding;
  }
}

export const DEFAULT_BRANDING: Branding = {
  site_name: "ThirtyFile",
  show_name: true,
  light_brand: "#2563eb",
  dark_brand: "#4f8bff",
  default_mode: "system",
  allow_toggle: true,
  login_title: "",
  login_subtitle: "Sign in to access your files", // must match the server default; displayed through tServer
  login_footer: "",
  login_lock: true,
  has_login_background: false,
  has_logo: false,
  has_logo_dark: false,
  version: 0,
};

export function useBranding(): Branding {
  const q = useQuery({
    queryKey: ["branding"],
    queryFn: api.branding,
    initialData: window.__TF_BRANDING__,
    staleTime: 5 * 60_000,
  });
  return q.data ?? DEFAULT_BRANDING;
}

export const logoUrl = (b: Branding, dark = false) => `/api/branding/logo?v=${b.version}${dark ? "&dark=1" : ""}`;
export const backgroundUrl = (b: Branding) => `/api/branding/background?v=${b.version}`;

/** Text color on the accent color: above a luminance of 0.19 black text has more contrast than white (same formula as the server) */
export function brandForeground(hex: string) {
  const ch = (i: number) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  const l = 0.2126 * ch(1) + 0.7152 * ch(3) + 0.0722 * ch(5);
  return l > 0.19 ? "#111111" : "#ffffff";
}

/** Apply to the current page after settings change: color stylesheet, favicon, appearance mode */
export function useApplyBranding() {
  const b = useBranding();
  useEffect(() => {
    let css = document.getElementById("tf-brand-css") as HTMLLinkElement | null;
    if (!css) {
      css = Object.assign(document.createElement("link"), {
        rel: "stylesheet",
        id: "tf-brand-css",
      });
      document.head.appendChild(css);
    }
    const href = `/api/branding.css?v=${b.version}`;
    if (css.getAttribute("href") !== href) css.href = href;

    const icon = document.querySelector<HTMLLinkElement>("link[rel=icon]");
    if (icon) {
      icon.removeAttribute("type");
      icon.href = b.has_logo ? logoUrl(b) : "/favicon.svg";
    }
    setThemePolicy(b.default_mode, b.allow_toggle);
  }, [b]);
}
