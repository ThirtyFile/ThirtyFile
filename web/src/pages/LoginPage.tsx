import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowRightIcon, KeyRoundIcon, Loader2Icon, UserRoundIcon } from "lucide-react";
import { api } from "@/api";
import { backgroundUrl, logoUrl, useBranding, type Branding } from "@/lib/branding";
import { SiteName } from "@/components/SiteName";
import { ProviderIcon } from "@/components/ProviderIcon";
import { LanguageSwitch } from "@/components/LanguageSwitch";
import { cn } from "@/lib/utils";
import { lang, locale, t, tServer } from "@/lib/i18n";

/** Account last used for a password sign-in (like an OS, next time only the password is needed) */
const LAST_USER = "tf-last-user";
/** Return to the lock screen after the sign-in screen has been idle this long (with no password typed) */
const IDLE_MS = 60_000;

function loadLastUser() {
  try {
    return localStorage.getItem(LAST_USER) || null;
  } catch {
    return null;
  }
}

function saveLastUser(username: string) {
  try {
    localStorage.setItem(LAST_USER, username);
  } catch {
    // If it can't be stored, ask for the account next time instead
  }
}

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

/** Current time, updated on every full minute */
function useNow() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    let timer = 0;
    const schedule = () => {
      timer = window.setTimeout(() => {
        setNow(new Date());
        schedule();
      }, 60_000 - (Date.now() % 60_000) + 50);
    };
    schedule();
    return () => window.clearTimeout(timer);
  }, []);
  return now;
}

const isDark = () => document.documentElement.classList.contains("dark");

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
    <span
      className={cn("grid shrink-0 place-items-center rounded-full bg-white/15 font-light text-white ring-1 ring-white/25 backdrop-blur-md select-none", className)}
      aria-hidden="true"
    >
      {name ? [...name][0].toUpperCase() : <UserRoundIcon className={iconClassName} strokeWidth={1.5} />}
    </span>
  );
}

const FIELD =
  "h-10 w-full rounded-md border border-white/25 bg-black/30 px-3 text-sm text-white outline-none backdrop-blur-md transition placeholder:text-white/65 focus:border-white/50 focus:bg-black/45 focus:shadow-[inset_0_-2px_0_var(--brand)] disabled:opacity-60";

type Stage = "lock" | "signin" | "welcome";

export function LoginPage() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const qc = useQueryClient();
  const b = useBranding();
  const providers = useQuery({ queryKey: ["sso-providers"], queryFn: api.ssoProviders, staleTime: 60_000 });
  const nextPath = (() => {
    const next = params.get("next");
    // Only allow same-site paths (same rule as the server's safe_next): "//host" and "/\host" are treated by browsers as other sites
    return next?.startsWith("/") && !next.startsWith("//") && !next.startsWith("/\\") ? next : "/files";
  })();

  // When a third-party login fails, the server redirects back with the reason
  const [error, setError] = useState<string | null>(() => {
    const e = params.get("sso_error");
    return e ? tServer(e) : null;
  });
  const [stage, setStage] = useState<Stage>(() => (b.login_lock && !error ? "lock" : "signin"));
  const [lastUser] = useState(loadLastUser);
  /** Currently selected account; null = other user (type the account) */
  const [who, setWho] = useState<string | null>(lastUser);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const userRef = useRef<HTMLInputElement>(null);
  const pwRef = useRef<HTMLInputElement>(null);
  /** After dismissing the error message, retype the password directly */
  const retry = useRef(false);
  const now = useNow();
  const dark = isDark();

  const focusField = () => {
    requestAnimationFrame(() => {
      const el = who || (retry.current && username) ? pwRef.current : userRef.current;
      retry.current = false;
      el?.focus();
      // Continue from the first character typed on the lock screen
      el?.setSelectionRange(el.value.length, el.value.length);
    });
  };

  const unlock = (typed?: string) => {
    if (stage !== "lock") return;
    // Typing directly on the lock screen: the first character goes into the field to be filled (like an OS)
    if (typed) {
      if (who) setPassword(typed);
      else setUsername(typed);
    }
    setStage("signin");
  };

  // Lock screen: any key unlocks (except shortcuts and function keys)
  useEffect(() => {
    if (stage !== "lock") return;
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey || /^(F\d+|Shift|Control|Alt|Meta|CapsLock|Tab|Escape)$/.test(e.key)) return;
      // The language menu is outside the locked area and works with the keyboard
      if ((e.target as HTMLElement)?.closest?.("select")) return;
      e.preventDefault();
      unlock(e.key.length === 1 && e.key !== " " ? e.key : undefined);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // When entering the sign-in screen or switching accounts, put the cursor in the field to fill
  useEffect(() => {
    if (stage === "signin" && !error) focusField();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stage, who, error]);

  // Return to the lock screen after the sign-in screen has been idle for a while (with no password typed)
  useEffect(() => {
    if (!b.login_lock || stage !== "signin" || busy || password) return;
    let timer = window.setTimeout(() => setStage("lock"), IDLE_MS);
    const reset = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setStage("lock"), IDLE_MS);
    };
    const events = ["pointermove", "pointerdown", "keydown", "wheel"] as const;
    events.forEach((ev) => window.addEventListener(ev, reset, { passive: true }));
    return () => {
      window.clearTimeout(timer);
      events.forEach((ev) => window.removeEventListener(ev, reset));
    };
  }, [b.login_lock, stage, busy, password]);

  const onSigninKey = (e: ReactKeyboardEvent) => {
    if (e.key === "Escape" && b.login_lock && !busy) {
      setError(null);
      setPassword("");
      setStage("lock");
    }
  };

  const switchUser = (next: string | null) => {
    if (busy) return;
    setWho(next);
    setError(null);
    setPassword("");
    if (!next) setUsername("");
  };

  const dismissError = () => {
    retry.current = true;
    setError(null);
    setPassword("");
  };

  const account = (who ?? username).trim();
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!account || !password || busy) return;
    setBusy(true);
    setError(null);
    try {
      const me = await api.login(account, password);
      saveLastUser(me.username);
      setStage("welcome");
      // Show "Welcome" briefly before entering (same as an OS sign-in)
      await new Promise((r) => setTimeout(r, 700));
      qc.setQueryData(["me"], me);
      navigate(nextPath, { replace: true });
    } catch (err) {
      setError(err instanceof Error ? err.message : t("Sign-in failed"));
      setBusy(false);
    }
  };

  const shownUser = stage === "welcome" ? account : who;
  const heading = shownUser ?? (b.login_title || b.site_name);
  const subtitle = tServer(b.login_subtitle);
  const logo = b.has_logo ? logoUrl(b, true) : "/favicon.svg";
  const hint = t("Click or press any key to sign in");

  return (
    <div className="relative h-full min-h-[480px] overflow-hidden bg-black text-white select-none" onKeyDown={onSigninKey}>
      <LoginWallpaper b={b} dark={dark} blurred={stage === "lock" ? undefined : "scale-110 blur-2xl"} />
      <div className={cn("absolute inset-0 transition-colors duration-700", stage === "lock" ? "bg-black/15" : "bg-black/40")} />

      {/* Site name and logo */}
      <div className="absolute top-5 left-6 flex min-w-0 items-center gap-2 text-sm font-semibold drop-shadow">
        <img src={logo} alt={b.show_name ? "" : b.site_name} className={cn("h-7 w-auto max-w-40 object-contain", !b.has_logo && "aspect-square")} />
        {(b.show_name || !b.has_logo) && <SiteName name={b.site_name} accentClassName="text-[#a9c4ff]" />}
      </div>

      {/* Lock screen: clock and date */}
      <button
        type="button"
        aria-label={hint}
        inert={stage !== "lock"}
        onClick={() => unlock()}
        onWheel={() => unlock()}
        className={cn(
          "absolute inset-0 flex cursor-default flex-col items-center pt-[max(12vh,4.5rem)] outline-none transition-[opacity,translate] duration-500 ease-out",
          stage !== "lock" && "pointer-events-none -translate-y-16 opacity-0",
        )}
      >
        <time
          dateTime={now.toISOString()}
          className="text-[clamp(4.5rem,15vw,8.5rem)] leading-none font-semibold tracking-tight tabular-nums [text-shadow:0_2px_24px_rgb(0_0_0/0.25)]"
        >
          {formatClock(now)}
        </time>
        <span className="mt-4 text-[clamp(1.1rem,2.8vw,1.65rem)] font-medium [text-shadow:0_1px_12px_rgb(0_0_0/0.3)]">{formatDate(now)}</span>
        <span className="absolute bottom-12 animate-pulse text-sm text-white/80 motion-reduce:animate-none">{hint}</span>
      </button>

      {/* Sign-in screen */}
      <div
        inert={stage === "lock"}
        className={cn(
          "absolute inset-0 flex items-center justify-center overflow-y-auto px-4 pt-16 pb-28 transition-[opacity,scale] duration-500 ease-out",
          stage === "lock" && "pointer-events-none scale-95 opacity-0",
        )}
      >
        <form onSubmit={submit} className="flex w-full max-w-72 flex-col items-center text-center" aria-label={t("Sign in")}>
          <LoginAvatar name={shownUser} className="size-36 text-6xl sm:size-44 sm:text-7xl" iconClassName="size-20 sm:size-24" />
          <h1 className="mt-5 max-w-full truncate text-[1.75rem] font-semibold [text-shadow:0_1px_12px_rgb(0_0_0/0.3)]">{heading}</h1>
          {subtitle && <p className="mt-1 max-w-full text-sm whitespace-pre-line text-white/80">{subtitle}</p>}

          {stage === "welcome" ? (
            <div className="mt-6 flex items-center gap-3 text-lg" role="status">
              <Loader2Icon className="size-5 animate-spin" />
              {t("Welcome")}
            </div>
          ) : error ? (
            <div className="mt-6 grid w-full justify-items-center gap-4" role="alert">
              <p className="text-sm">{error}</p>
              <button
                type="button"
                autoFocus
                onClick={dismissError}
                className="h-9 min-w-28 rounded-md bg-white/15 px-4 text-sm ring-1 ring-white/30 backdrop-blur-md hover:bg-white/25 focus-visible:ring-2 focus-visible:ring-white/80 focus-visible:outline-none"
              >
                {t("OK")}
              </button>
            </div>
          ) : (
            <div className="mt-6 grid w-full gap-2.5">
              {who ? (
                // Lets password managers identify the account
                <input type="text" className="sr-only" tabIndex={-1} aria-hidden="true" autoComplete="username" value={who} readOnly />
              ) : (
                <input
                  ref={userRef}
                  className={FIELD}
                  placeholder={t("Username")}
                  aria-label={t("Username")}
                  autoComplete="username"
                  autoCapitalize="none"
                  spellCheck={false}
                  value={username}
                  disabled={busy}
                  onChange={(e) => setUsername(e.target.value)}
                />
              )}
              <div className="relative">
                <input
                  ref={pwRef}
                  type="password"
                  className={cn(FIELD, "pr-11")}
                  placeholder={t("Password")}
                  aria-label={t("Password")}
                  autoComplete="current-password"
                  value={password}
                  disabled={busy}
                  onChange={(e) => setPassword(e.target.value)}
                />
                <button
                  type="submit"
                  aria-label={t("Sign in")}
                  title={t("Sign in")}
                  disabled={busy || !account || !password}
                  className="absolute top-1 right-1 grid size-8 place-items-center rounded bg-brand text-brand-foreground hover:bg-brand/85 focus-visible:ring-2 focus-visible:ring-white/80 focus-visible:outline-none disabled:bg-white/15 disabled:text-white/60"
                >
                  {busy ? <Loader2Icon className="size-4 animate-spin" /> : <ArrowRightIcon className="size-4" />}
                </button>
              </div>
            </div>
          )}

          {stage !== "welcome" && !!providers.data?.length && (
            <div className="mt-7 grid justify-items-center gap-2">
              <span className="text-xs text-white/75">{t("Sign-in options")}</span>
              <div className="flex flex-wrap justify-center gap-2">
                <span className="grid size-10 place-items-center rounded-md bg-white/20 ring-1 ring-white/40" title={t("Password")} aria-hidden="true">
                  <KeyRoundIcon className="size-5" />
                </span>
                {providers.data.map((p) => (
                  <a
                    key={p.id}
                    href={api.ssoStartUrl(p.id, nextPath)}
                    title={t("Sign in with {provider}", { provider: p.label })}
                    aria-label={t("Sign in with {provider}", { provider: p.label })}
                    className="grid size-10 place-items-center rounded-md bg-white/10 ring-1 ring-white/15 backdrop-blur-md hover:bg-white/20 focus-visible:ring-2 focus-visible:ring-white/80 focus-visible:outline-none"
                  >
                    <ProviderIcon provider={p.id} className="size-5" />
                  </a>
                ))}
              </div>
            </div>
          )}

          {/* Phones: switch-account goes below the sign-in box */}
          {lastUser && stage !== "welcome" && (
            <button
              type="button"
              onClick={() => switchUser(who ? null : lastUser)}
              className="mt-6 text-sm text-white/85 underline-offset-4 hover:underline sm:hidden"
            >
              {who ? t("Other user") : t("Sign in as {name}", { name: lastUser })}
            </button>
          )}
        </form>
      </div>

      {/* Bottom left: account list */}
      {lastUser && stage === "signin" && (
        <div className="absolute bottom-6 left-4 hidden gap-1 sm:grid">
          {[lastUser, null].map((u) => (
            <button
              key={u ?? ""}
              type="button"
              aria-pressed={who === u}
              onClick={() => switchUser(u)}
              className={cn(
                "flex min-w-44 items-center gap-3 rounded-md px-3 py-2 text-left text-sm hover:bg-white/10 focus-visible:ring-2 focus-visible:ring-white/80 focus-visible:outline-none",
                who === u && "bg-white/15",
              )}
            >
              <LoginAvatar name={u} className="size-9 text-base" iconClassName="size-5" />
              <span className="truncate">{u ?? t("Other user")}</span>
            </button>
          ))}
        </div>
      )}

      {/* Footer text */}
      {b.login_footer && stage === "signin" && (
        <p className="absolute inset-x-0 bottom-6 mx-auto max-w-md px-4 text-center text-xs whitespace-pre-line text-white/75 max-sm:bottom-14 sm:max-w-sm">
          {b.login_footer}
        </p>
      )}

      {/* Bottom right: language */}
      <LanguageSwitch className="absolute right-5 bottom-5 text-white/85 drop-shadow [&_select:hover]:text-white" />
    </div>
  );
}
