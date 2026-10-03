// The interface style: which device the browser is on, and the style `auto` gives it (lib/style/device.ts)
import { describe, expect, test } from "vitest";
import { detectDevice, deviceHints, resolveStyle, styleChoice, type DeviceHints, type Style } from "@/lib/style/device";

const SAFARI_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
const CHROME_MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";
const CHROME_WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";
const FIREFOX_WINDOWS = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:131.0) Gecko/20100101 Firefox/131.0";
const FIREFOX_LINUX = "Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0";
const CHROMEBOOK = "Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";
const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
const OLD_IPAD = "Mozilla/5.0 (iPad; CPU OS 12_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/12.1 Mobile/15E148 Safari/604.1";
const ANDROID_PHONE = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36";
const ANDROID_TABLET = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36";

const hints = (userAgent: string, platform: string, more: Partial<DeviceHints> = {}): DeviceHints => ({ userAgent, platform, touchPoints: 0, ...more });
const BOTH: Style[] = ["windows", "mac"];

describe("the device, from what the browser says", () => {
  test.each([
    ["Safari on a Mac", hints(SAFARI_MAC, "MacIntel"), "mac", "desktop"],
    ["Chrome on a Mac, with client hints", hints(CHROME_MAC, "MacIntel", { hintPlatform: "macOS", hintMobile: false }), "mac", "desktop"],
    ["Chrome on Windows", hints(CHROME_WINDOWS, "Win32", { hintPlatform: "Windows", hintMobile: false }), "windows", "desktop"],
    ["Firefox on Windows", hints(FIREFOX_WINDOWS, "Win32"), "windows", "desktop"],
    // A touch screen doesn't make a Windows laptop a tablet
    ["a Windows laptop with a touch screen", hints(CHROME_WINDOWS, "Win32", { touchPoints: 10, hintPlatform: "Windows", hintMobile: false }), "windows", "desktop"],
    ["Firefox on Linux", hints(FIREFOX_LINUX, "Linux x86_64"), "linux", "desktop"],
    ["a Chromebook", hints(CHROMEBOOK, "Linux x86_64", { hintPlatform: "Chrome OS", hintMobile: false }), "chromeos", "desktop"],
    ["a Chromebook without client hints", hints(CHROMEBOOK, "Linux x86_64"), "chromeos", "desktop"],
    // An iPad says it is a Mac: only its touch screen tells them apart
    ["an iPad", hints(SAFARI_MAC, "MacIntel", { touchPoints: 5 }), "ios", "tablet"],
    ["an iPad before iPadOS 13", hints(OLD_IPAD, "iPad", { touchPoints: 5 }), "ios", "tablet"],
    ["an iPhone", hints(IPHONE, "iPhone", { touchPoints: 5 }), "ios", "phone"],
    ["an Android phone", hints(ANDROID_PHONE, "Linux armv81", { touchPoints: 5, hintPlatform: "Android", hintMobile: true }), "android", "phone"],
    ["an Android phone without client hints", hints(ANDROID_PHONE, "Linux armv81", { touchPoints: 5 }), "android", "phone"],
    ["an Android tablet", hints(ANDROID_TABLET, "Linux armv81", { touchPoints: 5, hintPlatform: "Android", hintMobile: false }), "android", "tablet"],
    // The client hints are believed over the user agent, which browsers freeze
    ["client hints over the user agent", hints("Mozilla/5.0", "", { hintPlatform: "macOS" }), "mac", "desktop"],
    ["a phone by its client hints alone", hints("Mozilla/5.0", "", { hintPlatform: "Unknown", hintMobile: true }), "other", "phone"],
    ["only the platform", hints("", "MacIntel"), "mac", "desktop"],
    ["nothing known", hints("", ""), "other", "desktop"],
  ])("%s", (_, h, os, form) => {
    expect(detectDevice(h)).toEqual({ os, form });
  });

  test("the browser's own description is read from navigator", () => {
    const nav = { userAgent: CHROME_MAC, platform: "MacIntel", maxTouchPoints: 0, userAgentData: { platform: "macOS", mobile: false } } as unknown as Navigator;
    expect(deviceHints(nav)).toEqual({ userAgent: CHROME_MAC, platform: "MacIntel", hintPlatform: "macOS", hintMobile: false, touchPoints: 0 });
    // Firefox and Safari don't send client hints
    const plain = { userAgent: FIREFOX_LINUX, platform: "Linux x86_64", maxTouchPoints: 0 } as unknown as Navigator;
    expect(deviceHints(plain)).toEqual({ userAgent: FIREFOX_LINUX, platform: "Linux x86_64", hintPlatform: undefined, hintMobile: undefined, touchPoints: 0 });
  });
});

describe("the style a device gets", () => {
  const mac = detectDevice(hints(SAFARI_MAC, "MacIntel"));
  const windows = detectDevice(hints(CHROME_WINDOWS, "Win32"));
  const linux = detectDevice(hints(FIREFOX_LINUX, "Linux x86_64"));
  const chromebook = detectDevice(hints(CHROMEBOOK, "Linux x86_64"));
  const ipad = detectDevice(hints(SAFARI_MAC, "MacIntel", { touchPoints: 5 }));
  const iphone = detectDevice(hints(IPHONE, "iPhone", { touchPoints: 5 }));
  const android = detectDevice(hints(ANDROID_PHONE, "Linux armv81", { touchPoints: 5 }));

  test("auto follows the operating system", () => {
    expect(resolveStyle("auto", mac, BOTH)).toEqual({ style: "mac", touch: false });
    expect(resolveStyle("auto", windows, BOTH)).toEqual({ style: "windows", touch: false });
    expect(resolveStyle("auto", linux, BOTH)).toEqual({ style: "windows", touch: false });
    expect(resolveStyle("auto", chromebook, BOTH)).toEqual({ style: "windows", touch: false });
    expect(resolveStyle("auto", detectDevice(hints("", "")), BOTH)).toEqual({ style: "windows", touch: false });
  });

  test("tablets and phones get the phone and tablet layout", () => {
    expect(resolveStyle("auto", ipad, BOTH)).toEqual({ style: "mac", touch: true });
    expect(resolveStyle("auto", iphone, BOTH)).toEqual({ style: "mac", touch: true });
    expect(resolveStyle("auto", android, BOTH)).toEqual({ style: "windows", touch: true });
    expect(resolveStyle("windows", ipad, BOTH)).toEqual({ style: "windows", touch: true });
  });

  test("a chosen style is used on any device", () => {
    expect(resolveStyle("windows", mac, BOTH).style).toBe("windows");
    expect(resolveStyle("mac", windows, BOTH).style).toBe("mac");
    expect(resolveStyle("mac", linux, BOTH).style).toBe("mac");
  });

  test("a style that doesn't exist yet falls back to the Windows style", () => {
    expect(resolveStyle("mac", windows, ["windows"]).style).toBe("windows");
    expect(resolveStyle("auto", mac, ["windows"])).toEqual({ style: "windows", touch: false });
    expect(resolveStyle("auto", ipad, ["windows"])).toEqual({ style: "windows", touch: true });
  });

  test("a stored choice it doesn't know is auto", () => {
    expect(styleChoice("mac")).toBe("mac");
    expect(styleChoice("windows")).toBe("windows");
    for (const v of [undefined, null, "", "Mac", "linux", 1]) expect(styleChoice(v)).toBe("auto");
  });
});
