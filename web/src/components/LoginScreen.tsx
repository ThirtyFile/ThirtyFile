//! What the sign-in page shows around the form: the clock, the wallpaper and the account's avatar (also previewed in Control panel › Branding)

import { useId } from "react";
import { UserRoundIcon } from "lucide-react";
import { backgroundUrl, type Branding } from "@/lib/branding";
import { cn } from "@/lib/utils";
import { lang, locale } from "@/lib/i18n";

/** Lock screen time: no AM/PM shown (same as an OS lock screen); English follows the region's clock */
export function formatClock(d: Date) {
  return new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", hourCycle: lang === "en" ? undefined : "h23" })
    .formatToParts(d)
    .filter((p) => p.type !== "dayPeriod")
    .map((p) => p.value)
    .join("")
    .trim();
}

export const formatDate = (d: Date) => new Intl.DateTimeFormat(locale, { weekday: "long", month: "long", day: "numeric" }).format(d);

/** Blend two #RRGGBB colors by ratio (amount = 0 gives a, 1 gives b) */
function mix(a: string, b: string, amount: number) {
  const ch = (hex: string, i: number) => parseInt(hex.slice(i, i + 2), 16);
  return `#${[1, 3, 5]
    .map((i) => Math.round(ch(a, i) + (ch(b, i) - ch(a, i)) * amount))
    .map((v) => v.toString(16).padStart(2, "0"))
    .join("")}`;
}

/**
 * Login page wallpaper: the uploaded background image if there is one, otherwise a petal-shaped gradient derived from the accent color.
 * `blurred` is the frosted-glass effect of the sign-in screen (classes, e.g. "scale-110 blur-2xl")
 */
export function LoginWallpaper({ b, dark, blurred }: { b: Branding; dark: boolean; blurred?: string }) {
  const id = useId();
  const raw = dark ? b.dark_brand : b.light_brand;
  const brand = /^#[0-9a-f]{6}$/i.test(raw) ? raw : "#2563eb";
  const deep = mix(brand, "#030712", 0.78);
  const light = mix(brand, "#ffffff", 0.5);
  return (
    <div className={cn("absolute inset-0 transition-[filter,transform] duration-700 ease-out", blurred)} aria-hidden="true">
      {b.has_login_background ? (
        <img src={backgroundUrl(b)} alt="" className="size-full object-cover" draggable={false} />
      ) : (
        <div className="size-full" style={{ background: `linear-gradient(160deg, ${deep}, ${mix(brand, "#0b1020", 0.45)} 55%, ${mix(brand, "#000000", 0.85)})` }}>
          <svg viewBox="0 0 160 100" preserveAspectRatio="xMidYMid slice" className="size-full">
            <defs>
              <linearGradient id={`${id}p`} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" stopColor={light} stopOpacity="0.95" />
                <stop offset="0.55" stopColor={brand} stopOpacity="0.7" />
                <stop offset="1" stopColor={deep} stopOpacity="0.1" />
              </linearGradient>
              <radialGradient id={`${id}g`}>
                <stop offset="0" stopColor={light} stopOpacity="0.5" />
                <stop offset="1" stopColor={brand} stopOpacity="0" />
              </radialGradient>
              <filter id={`${id}s`} x="-20%" y="-20%" width="140%" height="140%">
                <feGaussianBlur stdDeviation="0.5" />
              </filter>
            </defs>
            <circle cx="80" cy="58" r="58" fill={`url(#${id}g)`} />
            <g className="tf-bloom" filter={`url(#${id}s)`} style={{ mixBlendMode: "screen" }}>
              {[-66, -40, -14, 14, 40, 66].map((a) => (
                <ellipse key={a} cx="80" cy="44" rx="11" ry="33" transform={`rotate(${a} 80 78)`} fill={`url(#${id}p)`} opacity="0.75" />
              ))}
              <ellipse cx="80" cy="47" rx="9" ry="30" fill={`url(#${id}p)`} />
            </g>
          </svg>
        </div>
      )}
    </div>
  );
}

/** Round avatar: known accounts show the first character, other users show a person icon */
export function LoginAvatar({ name, className, iconClassName }: { name: string | null; className?: string; iconClassName?: string }) {
  return (
    <span className={cn("grid shrink-0 place-items-center rounded-full bg-white/15 font-light text-white ring-1 ring-white/25 backdrop-blur-md select-none", className)} aria-hidden="true">
      {name ? [...name][0].toUpperCase() : <UserRoundIcon className={iconClassName} strokeWidth={1.5} />}
    </span>
  );
}
