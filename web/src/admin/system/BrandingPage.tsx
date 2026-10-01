import { useRef, useState, type ReactNode } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CheckIcon, FolderIcon, ImageUpIcon, Loader2Icon, MonitorIcon, MoonIcon, SunIcon, Trash2Icon, TriangleAlertIcon, UndoIcon } from "lucide-react";
import { toast } from "sonner";
import { api, type BrandingReq } from "@/api";
import { keys } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { backgroundUrl, brandForeground, contrastRatio, DEFAULT_BRANDING, logoUrl, PAGE_BACKGROUND, useBranding, type Branding } from "@/lib/branding";
import { SiteName } from "@/components/SiteName";
import { formatClock, formatDate, LoginAvatar, LoginWallpaper } from "@/components/LoginScreen";
import { confirm } from "@/lib/confirm";
import type { ThemeMode } from "@/lib/theme";
import { cn } from "@/lib/utils";
import { t, tServer, tc } from "@/lib/i18n";
import { Section, SettingsFrame, Toggle } from "@/admin/SettingsFrame";

/** Preset color schemes: each has an accent color tuned separately for light and dark mode */
const PRESETS = [
  { name: t("Blue"), light: "#2563eb", dark: "#4f8bff" },
  { name: t("Indigo"), light: "#4f46e5", dark: "#818cf8" },
  { name: t("Purple"), light: "#7c3aed", dark: "#a78bfa" },
  { name: t("Pink"), light: "#db2777", dark: "#f472b6" },
  { name: t("Red"), light: "#dc2626", dark: "#f87171" },
  { name: t("Orange"), light: "#ea580c", dark: "#fb923c" },
  { name: t("Green"), light: "#16a34a", dark: "#4ade80" },
  { name: t("Teal"), light: "#0d9488", dark: "#2dd4bf" },
  { name: t("Graphite"), light: "#334155", dark: "#94a3b8" },
];

/** Light and dark background colors for the preview (same as style.css) */
const SURFACES = {
  light: {
    bg: "#ffffff",
    fg: "#1f2328",
    muted: "#667085",
    border: "#e4e7ec",
    side: "#f7f8fa",
  },
  dark: {
    bg: "#1c1c1e",
    fg: "#e8e8e6",
    muted: "#a2a29e",
    border: "#37373a",
    side: "#151516",
  },
};

const HEX = /^#[0-9a-f]{6}$/i;

const editable = (b: Branding): BrandingReq => ({
  site_name: b.site_name,
  show_name: b.show_name,
  light_brand: b.light_brand,
  dark_brand: b.dark_brand,
  default_mode: b.default_mode,
  allow_toggle: b.allow_toggle,
  login_title: b.login_title,
  login_subtitle: b.login_subtitle,
  login_footer: b.login_footer,
  login_lock: b.login_lock,
});

export function BrandingPage() {
  const qc = useQueryClient();
  const b = useBranding();
  return (
    <SettingsFrame item="branding" onRefresh={() => qc.invalidateQueries({ queryKey: keys.branding() })}>
      {/* Uploading a logo doesn't clear unsaved text and colors */}
      <BrandingForm saved={b} />
    </SettingsFrame>
  );
}

function BrandingForm({ saved }: { saved: Branding }) {
  const qc = useQueryClient();
  const [draft, setDraft] = useState<BrandingReq>(() => editable(saved));
  const set = (patch: Partial<BrandingReq>) => setDraft((d) => ({ ...d, ...patch }));
  const dirty = JSON.stringify(draft) !== JSON.stringify(editable(saved));
  const invalid = !draft.site_name.trim() || !HEX.test(draft.light_brand) || !HEX.test(draft.dark_brand);
  // For the preview: the uploaded logo together with unsaved text and colors
  const preview: Branding = { ...saved, ...draft };

  const save = useMutation({
    mutationFn: () => api.updateBranding(draft),
    onSuccess: (data) => {
      qc.setQueryData(keys.branding(), data);
      toast.success(t("Branding updated"));
    },
    onError: (e) => toast.error(e.message),
  });

  return (
    <>
      <Section title={t("Site name and logo")}>
        <div className="grid gap-5 p-4">
          <div className="grid gap-2 sm:max-w-sm">
            <Label htmlFor="brand-name">{t("Site name")}</Label>
            <Input id="brand-name" value={draft.site_name} maxLength={40} onChange={(e) => set({ site_name: e.target.value })} />
            <p className="text-xs text-muted-foreground">{t("Shown in browser tabs, on the sign-in page, and on public share pages.")}</p>
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <LogoSlot variant="light" saved={saved} />
            <LogoSlot variant="dark" saved={saved} />
          </div>
          <div className="flex items-start gap-4">
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-medium">{t("Show site name next to the logo")}</div>
              <p className="mt-0.5 text-xs text-muted-foreground">{t("Turn this off if the logo image already includes text. The name is always shown when no logo is uploaded.")}</p>
            </div>
            <Toggle label={t("Show site name next to the logo")} checked={draft.show_name} onChange={(v) => set({ show_name: v })} />
          </div>
        </div>
      </Section>

      <Section title={t("Theme colors")}>
        <div className="grid gap-5 p-4">
          <div className="grid gap-2">
            <span className="text-[13px] font-medium">{t("Preset colors")}</span>
            <div className="flex flex-wrap gap-2">
              {PRESETS.map((p) => {
                const active = p.light === draft.light_brand.toLowerCase() && p.dark === draft.dark_brand.toLowerCase();
                return (
                  <button
                    key={p.name}
                    type="button"
                    aria-pressed={active}
                    title={t("{name}: light {light}, dark {dark}", { name: p.name, light: p.light, dark: p.dark })}
                    onClick={() => set({ light_brand: p.light, dark_brand: p.dark })}
                    className={cn("flex items-center gap-2 rounded-lg border px-2.5 py-1.5 text-xs hover:bg-muted", active && "border-brand ring-2 ring-brand/30")}
                  >
                    <span className="flex">
                      <span className="size-4 rounded-full border border-black/10" style={{ background: p.light }} />
                      <span className="-ml-1.5 size-4 rounded-full border border-white/20" style={{ background: p.dark }} />
                    </span>
                    {p.name}
                    {active && <CheckIcon className="size-3.5 text-brand" />}
                  </button>
                );
              })}
            </div>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <ColorField label={t("Light mode accent color")} icon={SunIcon} value={draft.light_brand} onChange={(v) => set({ light_brand: v })} />
            <ColorField label={t("Dark mode accent color")} icon={MoonIcon} value={draft.dark_brand} onChange={(v) => set({ dark_brand: v })} />
          </div>
          <p className="-mt-2 text-xs text-muted-foreground">
            {t(
              "The accent color is used for buttons, selected items, links, and focus rings. Dark mode usually needs a slightly brighter color than light mode to stay legible. Button text is automatically set to black or white based on the accent color.",
            )}
          </p>
          <ContrastWarning light={draft.light_brand} dark={draft.dark_brand} />
          <div className="grid gap-3 sm:grid-cols-2">
            <ThemePreview b={preview} mode="light" />
            <ThemePreview b={preview} mode="dark" />
          </div>
        </div>
      </Section>

      <Section title={t("Light and dark mode")}>
        <div className="grid gap-5 p-4">
          <div className="grid gap-2">
            <span className="text-[13px] font-medium">{t("Default appearance")}</span>
            <div className="flex flex-wrap gap-2">
              {(
                [
                  ["system", t("Use system setting"), MonitorIcon],
                  ["light", t("Light"), SunIcon],
                  ["dark", t("Dark"), MoonIcon],
                ] as const
              ).map(([mode, label, Icon]) => (
                <button
                  key={mode}
                  type="button"
                  aria-pressed={draft.default_mode === mode}
                  onClick={() => set({ default_mode: mode as ThemeMode })}
                  className={cn(
                    "flex h-9 items-center gap-2 rounded-lg border px-3 text-[13px] hover:bg-muted",
                    draft.default_mode === mode && "border-brand bg-brand/5 text-brand ring-2 ring-brand/20",
                  )}
                >
                  <Icon className="size-4" /> {label}
                </button>
              ))}
            </div>
            <p className="text-xs text-muted-foreground">
              {t(
                "The appearance used the first time someone opens the site, or when they haven't chosen one. \"Use system setting\" switches automatically based on the computer's or phone's dark mode setting.",
              )}
            </p>
          </div>
          <div className="flex items-start gap-4">
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-medium">{t("Let users switch between light and dark")}</div>
              <p className="mt-0.5 text-xs text-muted-foreground">
                {t('When off, "Appearance" is removed from the user menu and everyone uses the default appearance above (previous personal choices no longer apply).')}
              </p>
            </div>
            <Toggle label={t("Let users switch between light and dark")} checked={draft.allow_toggle} onChange={(v) => set({ allow_toggle: v })} />
          </div>
        </div>
      </Section>

      <Section title={t("Sign-in page")}>
        <div className="grid gap-5 p-4 md:grid-cols-[1fr_300px]">
          <div className="grid content-start gap-4">
            <div className="grid gap-2">
              <Label htmlFor="login-title">{t("Title")}</Label>
              <Input
                id="login-title"
                value={draft.login_title}
                maxLength={60}
                placeholder={t("e.g. Welcome to the company drive (optional)")}
                onChange={(e) => set({ login_title: e.target.value })}
              />
            </div>
            <div className="grid gap-2">
              <Label htmlFor="login-subtitle">{t("Description")}</Label>
              <Textarea
                id="login-subtitle"
                rows={2}
                value={draft.login_subtitle}
                maxLength={200}
                placeholder={t("e.g. Sign in with your company account")}
                onChange={(e) => set({ login_subtitle: e.target.value })}
              />
            </div>
            <div className="grid gap-2">
              <Label htmlFor="login-footer">{t("Footer text")}</Label>
              <Textarea
                id="login-footer"
                rows={3}
                value={draft.login_footer}
                maxLength={500}
                placeholder={t("e.g.\nForgot your password or need an account? Contact IT (ext. 123)")}
                onChange={(e) => set({ login_footer: e.target.value })}
              />
              <p className="text-xs text-muted-foreground">{t("Shown at the bottom of the sign-in screen, e.g. contact details or usage rules. Line breaks are kept.")}</p>
            </div>
            <div className="flex items-start gap-4">
              <div className="min-w-0 flex-1">
                <div className="text-[13px] font-medium">{t("Show lock screen")}</div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t("Show the time and date first, like a computer's lock screen. The sign-in box appears after a click or any key press.")}
                </p>
              </div>
              <Toggle label={t("Show lock screen")} checked={draft.login_lock} onChange={(v) => set({ login_lock: v })} />
            </div>
            <BackgroundSlot saved={saved} />
          </div>
          <LoginPreview b={preview} />
        </div>
      </Section>

      <div className="sticky bottom-0 -mx-6 -mb-6 flex flex-wrap items-center gap-2 border-t bg-background/95 px-6 py-3 backdrop-blur">
        <span className="text-xs text-muted-foreground">
          {invalid
            ? t("Enter a site name. Accent colors must be in #RRGGBB format")
            : dirty
              ? t("You have unsaved changes")
              : t('Logos take effect as soon as they\'re uploaded; other settings take effect after you click "Save"')}
        </span>
        <span className="flex-1" />
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setDraft(editable(DEFAULT_BRANDING))}
          title={t("Reset text, colors, and appearance to defaults (logos are kept). Takes effect after saving")}
        >
          <UndoIcon /> {t("Restore defaults")}
        </Button>
        {dirty && (
          <Button variant="outline" size="sm" onClick={() => setDraft(editable(saved))}>
            {t("Discard changes")}
          </Button>
        )}
        <Button size="sm" disabled={!dirty || invalid || save.isPending} onClick={() => save.mutate()}>
          {save.isPending && <Loader2Icon className="animate-spin" />}
          {t("Save")}
        </Button>
      </div>
    </>
  );
}

function ColorField({ label, icon: Icon, value, onChange }: { label: string; icon: typeof SunIcon; value: string; onChange(v: string): void }) {
  const valid = HEX.test(value);
  return (
    <div className="grid gap-2">
      <Label className="flex items-center gap-1.5">
        <Icon className="size-3.5 text-muted-foreground" /> {label}
      </Label>
      <div className="flex items-center gap-2">
        <input
          type="color"
          aria-label={t("{label} (color picker)", { label })}
          value={valid ? value : "#000000"}
          onChange={(e) => onChange(e.target.value)}
          className="h-8 w-10 shrink-0 cursor-pointer rounded-md border bg-transparent p-0.5"
        />
        <Input
          aria-label={label}
          value={value}
          aria-invalid={!valid}
          maxLength={7}
          className="h-8 w-28 font-mono text-xs uppercase"
          onChange={(e) => {
            const v = e.target.value.trim();
            onChange(v.startsWith("#") ? v : `#${v}`);
          }}
        />
        {valid && (
          <span className="rounded px-2 py-1 text-[11px] font-medium" style={{ background: value, color: brandForeground(value) }}>
            {t("Button text")}
          </span>
        )}
      </div>
    </div>
  );
}

function LogoSlot({ variant, saved }: { variant: "light" | "dark"; saved: Branding }) {
  const qc = useQueryClient();
  const input = useRef<HTMLInputElement>(null);
  const dark = variant === "dark";
  const has = dark ? saved.has_logo_dark : saved.has_logo;
  const upload = useMutation({
    mutationFn: (f: File) => api.uploadLogo(variant, f),
    onSuccess: (data) => {
      qc.setQueryData(keys.branding(), data);
      toast.success(t("Logo updated"));
    },
    onError: (e) => toast.error(e.message),
  });
  const remove = useMutation({
    mutationFn: () => api.deleteLogo(variant),
    onSuccess: (data) => {
      qc.setQueryData(keys.branding(), data);
      toast.success(t("Logo removed"));
    },
    onError: (e) => toast.error(e.message),
  });
  const s = dark ? SURFACES.dark : SURFACES.light;
  // Without a dedicated dark-mode logo, reuse the light one
  const shown = has || (dark && saved.has_logo) ? logoUrl(saved, dark) : "/favicon.svg";
  return (
    <div className="grid content-start gap-2">
      <span className="flex items-center gap-1.5 text-[13px] font-medium">
        {dark ? <MoonIcon className="size-3.5 text-muted-foreground" /> : <SunIcon className="size-3.5 text-muted-foreground" />}
        {dark ? t("Dark mode logo") : "Logo"}
      </span>
      <div className="flex h-20 items-center justify-center rounded-lg border px-4" style={{ background: s.bg }}>
        <img src={shown} alt="" className={cn("max-h-12 max-w-full object-contain", !has && !(dark && saved.has_logo) && "opacity-40")} />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <input
          ref={input}
          type="file"
          hidden
          accept="image/png,image/jpeg,image/svg+xml,image/webp,image/gif,image/x-icon,.ico"
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            if (f) upload.mutate(f);
          }}
        />
        <Button variant="outline" size="sm" disabled={upload.isPending} onClick={() => input.current?.click()}>
          {upload.isPending ? <Loader2Icon className="animate-spin" /> : <ImageUpIcon />}
          {has ? t("Replace") : t("Upload")}
        </Button>
        {has && (
          <Button
            variant="ghost"
            size="sm"
            disabled={remove.isPending}
            onClick={async () => {
              const ok = await confirm({
                title: dark ? t("Remove the dark mode logo?") : t("Remove the logo?"),
                description: t("The image is deleted. To use it again, you'll need to upload it again."),
                confirmText: t("Remove"),
                destructive: true,
              });
              if (ok) remove.mutate();
            }}
          >
            <Trash2Icon /> {t("Remove")}
          </Button>
        )}
        <span className="text-[11px] text-muted-foreground">
          {dark
            ? has
              ? t("For dark backgrounds")
              : saved.has_logo
                ? t("Not set; the light logo is used")
                : t("Optional")
            : t("PNG, SVG, JPG, or WebP, up to 1 MB. Also used as the site icon")}
        </span>
      </div>
    </div>
  );
}

/** Scaled-down view in light or dark mode (colors are set directly on the elements, unaffected by the current page's appearance) */
function ThemePreview({ b, mode }: { b: Branding; mode: "light" | "dark" }) {
  const s = SURFACES[mode];
  const brand = HEX.test(mode === "dark" ? b.dark_brand : b.light_brand) ? (mode === "dark" ? b.dark_brand : b.light_brand) : "#888888";
  const selection = `color-mix(in srgb, ${brand} ${mode === "dark" ? 24 : 13}%, ${s.bg})`;
  const logo = (b.has_logo_dark && mode === "dark") || b.has_logo ? logoUrl(b, mode === "dark") : "/favicon.svg";
  const row = (label: string, selected = false) => (
    <div className="flex items-center gap-1.5 rounded px-1.5 py-1" style={selected ? { background: selection, boxShadow: `inset 3px 0 0 ${brand}` } : undefined}>
      <FolderIcon className="size-3" style={{ color: selected ? brand : s.muted }} />
      <span style={{ color: selected ? brand : s.fg }}>{label}</span>
    </div>
  );
  return (
    <figure className="grid gap-1.5">
      <div className="flex h-40 overflow-hidden rounded-lg border text-[10px]" style={{ background: s.bg, color: s.fg, borderColor: s.border }}>
        <div className="grid w-24 shrink-0 content-start gap-0.5 border-r p-1.5" style={{ background: s.side, borderColor: s.border }}>
          <div className="mb-1 flex min-w-0 items-center gap-1 font-semibold">
            <img src={logo} alt="" className="h-3.5 w-auto max-w-12 object-contain" />
            {(b.show_name || !b.has_logo) && <SiteName name={b.site_name || t("Site name")} accentColor={brand} />}
          </div>
          {row(t("My files"), true)}
          {row(t("Shared with me"))}
          {row(t("Trash"))}
        </div>
        <div className="grid min-w-0 flex-1 content-start gap-1 p-2">
          <div className="flex items-center gap-1.5">
            <span className="rounded px-2 py-0.5 font-medium" style={{ background: brand, color: brandForeground(brand) }}>
              {t("Upload")}
            </span>
            <span className="rounded border px-2 py-0.5" style={{ borderColor: s.border }}>
              {t("New")}
            </span>
          </div>
          <div className="rounded border px-1.5 py-1" style={{ borderColor: s.border }}>
            {row(t("Annual report"))}
            {row(tc("example", "Project files"), true)}
            {row(t("Photos"))}
          </div>
          <span style={{ color: brand }} className="underline">
            {t("Share links")}
          </span>
        </div>
      </div>
      <figcaption className="flex items-center gap-1 text-[11px] text-muted-foreground">
        {mode === "dark" ? <MoonIcon className="size-3" /> : <SunIcon className="size-3" />}
        {mode === "dark" ? t("Dark mode") : t("Light mode")}
      </figcaption>
    </figure>
  );
}

/** Login page background image: takes effect immediately after upload */
function BackgroundSlot({ saved }: { saved: Branding }) {
  const qc = useQueryClient();
  const input = useRef<HTMLInputElement>(null);
  const done = (msg: string) => (data: Branding) => {
    qc.setQueryData(keys.branding(), data);
    toast.success(msg);
  };
  const upload = useMutation({
    mutationFn: (f: File) => api.uploadLoginBackground(f),
    onSuccess: done(t("Sign-in background updated")),
    onError: (e) => toast.error(e.message),
  });
  const remove = useMutation({
    mutationFn: () => api.deleteLoginBackground(),
    onSuccess: done(t("Sign-in background removed")),
    onError: (e) => toast.error(e.message),
  });
  const has = saved.has_login_background;
  return (
    <div className="grid gap-2">
      <span className="text-[13px] font-medium">{t("Background image")}</span>
      <div className="flex flex-wrap items-center gap-3">
        <div className="relative aspect-[16/10] w-28 shrink-0 overflow-hidden rounded-md border bg-black">
          {has ? <img src={backgroundUrl(saved)} alt="" className="size-full object-cover" /> : <LoginWallpaper b={saved} dark={isDark()} />}
        </div>
        <div className="grid gap-1.5">
          <div className="flex flex-wrap items-center gap-2">
            <input
              ref={input}
              type="file"
              hidden
              accept="image/png,image/jpeg,image/webp,image/gif"
              onChange={(e) => {
                const f = e.target.files?.[0];
                e.target.value = "";
                if (f) upload.mutate(f);
              }}
            />
            <Button variant="outline" size="sm" disabled={upload.isPending} onClick={() => input.current?.click()}>
              {upload.isPending ? <Loader2Icon className="animate-spin" /> : <ImageUpIcon />}
              {has ? t("Replace") : t("Upload")}
            </Button>
            {has && (
              <Button
                variant="ghost"
                size="sm"
                disabled={remove.isPending}
                onClick={async () => {
                  const ok = await confirm({
                    title: t("Remove the background image?"),
                    description: t("The image is deleted. To use it again, you'll need to upload it again."),
                    confirmText: t("Remove"),
                    destructive: true,
                  });
                  if (ok) remove.mutate();
                }}
              >
                <Trash2Icon /> {t("Remove")}
              </Button>
            )}
          </div>
          <span className="text-[11px] text-muted-foreground">
            {has ? t("PNG, JPG or WebP, up to 5 MB; 1920×1080 or larger recommended") : t("Not set: generated from the brand color. PNG, JPG or WebP, up to 5 MB")}
          </span>
        </div>
      </div>
    </div>
  );
}

const isDark = () => document.documentElement.classList.contains("dark");

/** Scaled-down preview of the login page: lock screen and sign-in screen (accent color follows the current page's appearance) */
function LoginPreview({ b }: { b: Branding }) {
  const dark = isDark();
  const now = new Date();
  const frame = (label: string, body: ReactNode, blurred?: string) => (
    <figure className="grid gap-1">
      <div className="relative aspect-[16/10] overflow-hidden rounded-lg border bg-black text-white select-none">
        <LoginWallpaper b={b} dark={dark} blurred={blurred} />
        <div className={cn("absolute inset-0", blurred ? "bg-black/40" : "bg-black/15")} />
        {body}
      </div>
      <figcaption className="text-[11px] text-muted-foreground">{label}</figcaption>
    </figure>
  );
  const brand = dark ? b.dark_brand : b.light_brand;
  return (
    <div className="grid w-full max-w-sm content-start gap-3">
      {b.login_lock &&
        frame(
          t("Lock screen"),
          <div className="absolute inset-0 flex flex-col items-center pt-[13%]">
            <span className="text-4xl leading-none font-semibold tabular-nums">{formatClock(now)}</span>
            <span className="mt-1.5 text-[11px] font-medium">{formatDate(now)}</span>
          </div>,
        )}
      {frame(
        t("Sign-in screen"),
        <div className="absolute inset-0 flex flex-col items-center justify-center px-6 text-center">
          <LoginAvatar name={null} className="size-11" iconClassName="size-6" />
          <span className="mt-1.5 max-w-full truncate text-xs font-semibold">{b.login_title || b.site_name || t("Site name")}</span>
          {b.login_subtitle && <span className="max-w-full truncate text-[8px] text-white/80">{tServer(b.login_subtitle)}</span>}
          <span className="mt-1.5 h-3.5 w-28 rounded-sm border border-white/25 bg-black/30" />
          <span className="mt-1 flex h-3.5 w-28 justify-end rounded-sm border border-white/25 bg-black/30 p-px">
            <span className="aspect-square h-full rounded-[2px]" style={{ background: HEX.test(brand) ? brand : undefined }} />
          </span>
          {b.login_footer && <span className="absolute inset-x-0 bottom-1.5 truncate px-4 text-[7px] text-white/75">{b.login_footer}</span>}
        </div>,
        "scale-110 blur-sm",
      )}
    </div>
  );
}

/** Focus rings, selection bars and links need at least 3:1 against the page (WCAG 1.4.11); text on the accent needs 4.5:1 */
function ContrastWarning({ light, dark }: { light: string; dark: string }) {
  const valid = (c: string) => /^#[0-9a-f]{6}$/i.test(c);
  const problems = (
    [
      [light, PAGE_BACKGROUND.light, t("light mode")],
      [dark, PAGE_BACKGROUND.dark, t("dark mode")],
    ] as const
  ).flatMap(([color, page, mode]) => {
    if (!valid(color)) return [];
    const page_ = contrastRatio(color, page);
    const text = contrastRatio(color, brandForeground(color));
    const out = [];
    if (page_ < 3)
      out.push(
        t("In {mode}, the accent color stands out too little from the page ({ratio}:1, at least 3:1 is needed): focus rings and selected items are hard to see.", {
          mode,
          ratio: page_.toFixed(2),
        }),
      );
    if (text < 4.5) out.push(t("In {mode}, button text on the accent color is hard to read ({ratio}:1, at least 4.5:1 is needed).", { mode, ratio: text.toFixed(2) }));
    return out;
  });
  if (!problems.length) return null;
  return (
    <div role="status" className="-mt-1 flex gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-2.5 text-xs">
      <TriangleAlertIcon className="size-4 shrink-0 text-amber-600" aria-hidden="true" />
      <div className="grid gap-1">
        {problems.map((p) => (
          <p key={p}>{p}</p>
        ))}
      </div>
    </div>
  );
}
