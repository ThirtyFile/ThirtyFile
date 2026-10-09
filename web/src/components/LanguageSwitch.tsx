import { LanguagesIcon } from "lucide-react";
import { api } from "@/api";
import { LANGS, lang, setLang, t, type Lang } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Language switcher (login page, public share page; reloads the page after switching). Someone signed in (on a share
 * page) also gets it saved with their account, as in the account menu; on the sign-in page nobody is (`signedOut`), so
 * nothing is asked of the server. `beforeReload`: what the page keeps for after the reload. */
export function LanguageSwitch({ className, signedOut, beforeReload }: { className?: string; signedOut?: boolean; beforeReload?: () => void }) {
  return (
    <label className={cn("inline-flex items-center gap-1.5 text-xs text-muted-foreground", className)}>
      <LanguagesIcon className="size-3.5" aria-hidden="true" />
      <select
        // In the page's language, and in English too, for someone who landed on a language they can't read
        aria-label={lang === "en" ? "Language" : `${t("Language")} / Language`}
        className="cursor-pointer rounded bg-transparent py-0.5 outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [&>option]:bg-popover [&>option]:text-popover-foreground"
        value={lang}
        onChange={(e) => {
          const next = e.target.value as Lang;
          const switchTo = () => {
            beforeReload?.();
            setLang(next);
          };
          if (signedOut) switchTo();
          else void api.setLanguageIfSignedIn(next).then(switchTo);
        }}
      >
        {LANGS.map((l) => (
          <option key={l.id} value={l.id}>
            {l.label}
          </option>
        ))}
      </select>
    </label>
  );
}
