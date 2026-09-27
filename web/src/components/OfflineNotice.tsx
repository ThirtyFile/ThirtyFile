import { CloudOffIcon, RefreshCwIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, tServer } from "@/lib/i18n";

/** Notice above the file list when the space's storage service is offline */
export function OfflineBanner({ reason }: { reason: string }) {
  return (
    <div className="flex shrink-0 items-start gap-2 border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-800 dark:text-amber-200">
      <CloudOffIcon className="mt-px size-4 shrink-0" />
      <div className="min-w-0">
        <span className="font-medium">{t("This space's storage service is offline.")}</span>{" "}
        {t("You can browse the file list and organize folders, but you can't open, download, or upload files for now. Everything resumes automatically once the connection is restored.")}
        <span className="ml-1 text-amber-700/80 dark:text-amber-300/70">{t("({reason})", { reason: tServer(reason) })}</span>
      </div>
    </div>
  );
}

/** The storage service holding the opened file is offline */
export function OfflinePanel({ reason, retrying, onRetry }: { reason: string; retrying?: boolean; onRetry(): void }) {
  return (
    <div className="flex max-w-md flex-col items-center gap-3 p-6 text-center">
      <CloudOffIcon className="size-10 text-muted-foreground" />
      <div>
        <div className="font-medium">{t("The storage service for this file is offline")}</div>
        <p className="mt-1 text-sm text-muted-foreground">{t("You can open it once the connection is restored. This page will reload automatically.")}</p>
        <p className="mt-1 text-xs break-all text-muted-foreground/80">{tServer(reason)}</p>
      </div>
      <Button variant="outline" size="sm" disabled={retrying} onClick={onRetry}>
        <RefreshCwIcon className={retrying ? "animate-spin" : undefined} /> {t("Retry")}
      </Button>
    </div>
  );
}
