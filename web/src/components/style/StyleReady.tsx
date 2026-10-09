/**
 * Waits for the style's parts before showing what uses them: the Mac style's are a chunk of their own
 * (components/style). A page shown with stand-ins first would keep what it chose from them, such as the view a style
 * starts in.
 */
import type { ReactNode } from "react";
import { Loader2Icon } from "lucide-react";
import { useStyleReady } from "@/components/style";
import { t } from "@/lib/i18n";

export function StyleReady({ children }: { children: ReactNode }) {
  if (useStyleReady()) return children;
  return (
    <div className="flex h-full items-center justify-center text-muted-foreground" role="status" aria-label={t("Loading…")}>
      <Loader2Icon className="size-6 animate-spin" />
    </div>
  );
}
