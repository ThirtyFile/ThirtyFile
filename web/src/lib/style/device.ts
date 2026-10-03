/**
 * Which interface style a page uses: the person's choice (saved with their account), with `auto` decided here, in the
 * browser, by the device's operating system. Plain functions of what the browser says about itself, so they can be
 * tested without one.
 */

/** What a person can choose: `auto` follows the operating system of the device in use */
export type StyleChoice = "auto" | "windows" | "mac";
export const STYLE_CHOICES: readonly StyleChoice[] = ["auto", "windows", "mac"];

/** The styles themselves */
export type Style = "windows" | "mac";

/** What the browser says about the device it runs on */
export interface DeviceHints {
  userAgent: string;
  /** `navigator.platform`, still filled in everywhere ("MacIntel", "Win32", "Linux x86_64", "iPhone"…) */
  platform: string;
  /** The user-agent client hints of Chromium browsers (`navigator.userAgentData`): "macOS", "Windows", "Android"… */
  hintPlatform?: string;
  /** The same: whether the browser counts the device as a phone */
  hintMobile?: boolean;
  /** `navigator.maxTouchPoints`: 0 on a Mac, more on an iPad (which says it is a Mac) */
  touchPoints: number;
}

export type Os = "mac" | "windows" | "linux" | "chromeos" | "ios" | "android" | "other";

/** A computer, a tablet or a phone */
export type Form = "desktop" | "tablet" | "phone";

export interface Device {
  os: Os;
  form: Form;
}

const HINTED: Record<string, Os> = {
  macos: "mac",
  windows: "windows",
  linux: "linux",
  "chrome os": "chromeos",
  "chromium os": "chromeos",
  chromeos: "chromeos",
  android: "android",
  ios: "ios",
  ipados: "ios",
};

function osOf(h: DeviceHints): Os {
  const hinted = h.hintPlatform && HINTED[h.hintPlatform.trim().toLowerCase()];
  if (hinted) return hinted;
  const ua = h.userAgent;
  // Android and ChromeOS also say "Linux"; iPads since iPadOS 13 say "Macintosh" (told apart below)
  if (/iPhone|iPad|iPod/.test(ua)) return "ios";
  if (/Android/.test(ua)) return "android";
  if (/\bCrOS\b/.test(ua)) return "chromeos";
  if (/Windows/.test(ua)) return "windows";
  if (/Macintosh|Mac OS X/.test(ua)) return "mac";
  if (/Linux|X11/.test(ua)) return "linux";
  const p = h.platform;
  if (/iPhone|iPad|iPod/.test(p)) return "ios";
  if (p.startsWith("Mac")) return "mac";
  if (p.startsWith("Win")) return "windows";
  if (/Linux/.test(p)) return "linux";
  return "other";
}

/** The device's operating system and whether it is a computer, a tablet or a phone */
export function detectDevice(h: DeviceHints): Device {
  let os = osOf(h);
  // An iPad asks for the pages of a Mac, and says so; only a Mac has no touch screen
  if (os === "mac" && h.touchPoints > 1) os = "ios";
  const phone = h.hintMobile === true || (os === "ios" && /iPhone|iPod/.test(h.userAgent + h.platform)) || (os === "android" && /Mobile/.test(h.userAgent));
  const form: Form = phone ? "phone" : os === "ios" || os === "android" ? "tablet" : "desktop";
  return { os, form };
}

/** What a page uses */
export interface Resolved {
  /** The style: its keys, menus, icons and views */
  style: Style;
  /**
   * The phone and tablet layout, the same in every style, rather than a computer's. Today that is the layout of a
   * narrow window, which every device gets when its window is narrow
   */
  touch: boolean;
}

/**
 * The style a device gets for a choice. `auto`: the Mac style on a Mac (and an iPad, or an iPhone), the Windows style
 * on Windows, Linux, ChromeOS and anything else. A style not among those `ready` falls back to the Windows style.
 */
export function resolveStyle(choice: StyleChoice, device: Device, ready: readonly Style[]): Resolved {
  const wanted: Style = choice !== "auto" ? choice : device.os === "mac" || device.os === "ios" ? "mac" : "windows";
  return { style: ready.includes(wanted) ? wanted : "windows", touch: device.form !== "desktop" };
}

/** The choice as stored, or `auto` for anything else (an older server, a value from a newer one) */
export function styleChoice(value: unknown): StyleChoice {
  return STYLE_CHOICES.includes(value as StyleChoice) ? (value as StyleChoice) : "auto";
}

interface UserAgentData {
  platform?: string;
  mobile?: boolean;
}

/** What this browser says about its device */
export function deviceHints(nav: Navigator = navigator): DeviceHints {
  const data = (nav as Navigator & { userAgentData?: UserAgentData }).userAgentData;
  return {
    userAgent: nav.userAgent ?? "",
    platform: nav.platform ?? "",
    hintPlatform: data?.platform || undefined,
    hintMobile: data?.mobile,
    touchPoints: nav.maxTouchPoints ?? 0,
  };
}

/** This device, as the browser describes it when the page loads */
export const thisDevice: Device = detectDevice(typeof navigator === "undefined" ? { userAgent: "", platform: "", touchPoints: 0 } : deviceHints());
