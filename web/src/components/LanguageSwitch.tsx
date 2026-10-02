import { LanguagesIcon } from "lucide-react";
import { api } from "@/api";
import { LANGS, lang, setLang, type Lang } from "@/lib/i18n";
import { cn } from "@/lib/utils";

/** Language switcher (login page, public share page; reloads the page after switching). Someone signed in (on a share
 * page) also gets it saved with their account, as in the account menu */
export function LanguageSwitch({ className }: { className?: string }) {
  return (
    <label className={cn("inline-flex items-center gap-1.5 text-xs text-muted-foreground", className)}>
      <LanguagesIcon className="size-3.5" aria-hidden="true" />
      <select
        aria-label="語言 / Language" // i18n-ignore: intentionally bilingual so it is understood in either language
        className="cursor-pointer rounded bg-transparent py-0.5 outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring [&>option]:bg-popover [&>option]:text-popover-foreground"
        value={lang}
        onChange={(e) => {
          const next = e.target.value as Lang;
          void api.setLanguageIfSignedIn(next).then(() => setLang(next));
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
