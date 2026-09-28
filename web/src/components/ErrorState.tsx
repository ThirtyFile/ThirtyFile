import { useState } from "react";
import { CircleAlertIcon, RefreshCwIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { t } from "@/lib/i18n";

/**
 * Something couldn't be loaded: the message and a Try again button.
 * onRetry may return a promise (e.g. a query's refetch); the button spins until it settles.
 * compact: one line, for small places such as a dialog or the details pane
 */
export function ErrorState({ message, onRetry, compact, className }: { message: string; onRetry(): unknown; compact?: boolean; className?: string }) {
  const [busy, setBusy] = useState(false);
  const retry = async () => {
    setBusy(true);
    try {
      await onRetry();
    } catch {
      // the query shows the new error itself
    } finally {
      setBusy(false);
    }
  };
  const button = (
    <Button type="button" variant="outline" size="sm" disabled={busy} onClick={retry}>
      <RefreshCwIcon className={busy ? "animate-spin" : undefined} /> {t("Try again")}
    </Button>
  );

  if (compact) {
    return (
      <div role="alert" className={cn("flex flex-wrap items-center gap-2 text-sm", className)}>
        <CircleAlertIcon className="size-4 shrink-0 text-destructive" />
        <span className="min-w-0 flex-1 break-words text-destructive">{message}</span>
        {button}
      </div>
    );
  }
  return (
    <div role="alert" className={cn("flex min-h-52 flex-col items-center justify-center gap-3 p-6 text-center", className)}>
      <CircleAlertIcon className="size-9 stroke-[1.4] text-destructive" />
      <p className="max-w-md text-sm break-words text-destructive">{message}</p>
      {button}
    </div>
  );
}
