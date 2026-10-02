import { useState, type ReactNode } from "react";
import { Link } from "react-router";
import { CircleAlertIcon, LayersIcon, RefreshCwIcon, SearchXIcon } from "lucide-react";
import { ApiError } from "@/api/client";
import { Button, buttonVariants } from "@/components/ui/button";
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

/**
 * In place of something that hasn't loaded yet: `loading` while its query is still trying, or the error and Try again
 * once it failed (a failed load mustn't look like an empty list, or keep loading for ever)
 */
export function Pending({ query, loading, compact, className }: { query: { error: Error | null; refetch(): unknown }; loading: ReactNode; compact?: boolean; className?: string }) {
  if (!query.error) return loading;
  return <ErrorState compact={compact} className={className} message={query.error.message} onRetry={() => query.refetch()} />;
}

/**
 * A folder or file that couldn't be opened. When it isn't there, or isn't there for this person (the server answers
 * "not found" for items someone can't see), trying again doesn't help: this says what happened and offers All
 * spaces. Any other error shows with Try again.
 */
export function ItemError({ error, kind, onRetry, className }: { error: Error; kind: "folder" | "file"; onRetry(): unknown; className?: string }) {
  const status = error instanceof ApiError ? error.status : 0;
  if (status !== 404 && status !== 403) return <ErrorState message={error.message} onRetry={onRetry} className={className} />;
  const title =
    status === 404
      ? kind === "folder"
        ? t("This folder can't be found")
        : t("This file can't be found")
      : kind === "folder"
        ? t("You don't have access to this folder")
        : t("You don't have access to this file");
  return (
    <div role="alert" className={cn("flex min-h-52 flex-col items-center justify-center gap-3 p-6 text-center", className)}>
      <SearchXIcon className="size-9 stroke-[1.4] text-muted-foreground" />
      <div className="grid max-w-md gap-1">
        <p className="font-medium">{title}</p>
        <p className="text-sm text-muted-foreground">
          {status === 404
            ? t("It may have been deleted or moved, or you may no longer have access to it.")
            : t("Ask the person who shared it with you, or a manager of its space, for access.")}
        </p>
      </div>
      <Link to="/drives" className={buttonVariants({ variant: "outline", size: "sm" })}>
        <LayersIcon /> {t("Go to All spaces")}
      </Link>
    </div>
  );
}
