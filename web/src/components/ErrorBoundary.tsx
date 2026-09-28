import { Component, type ErrorInfo, type ReactNode } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { isChunkLoadError, reloadForNewVersion } from "@/lib/reload";
import { t } from "@/lib/i18n";

/**
 * Show an explanation when the UI hits an unexpected error, instead of the whole app going blank.
 * The error state is cleared automatically when resetKey changes (e.g. switching pages).
 */
export class ErrorBoundary extends Component<{ children: ReactNode; resetKey?: string }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The site was updated and the old code chunk no longer exists: reload automatically
    if (isChunkLoadError(error) && reloadForNewVersion()) return;
    console.error("Render error", error, info.componentStack);
  }

  // A new reset key (another page or file) clears the error
  componentDidUpdate(prev: { resetKey?: string }) {
    // oxlint-disable-next-line react/no-did-update-set-state -- only when the key changed, so it can't loop
    if (this.state.error && prev.resetKey !== this.props.resetKey) this.setState({ error: null });
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    if (isChunkLoadError(error)) {
      return (
        <div className="flex h-full flex-col items-center justify-center gap-4 p-6 text-center">
          <div>
            <div className="font-medium">{t("The site has been updated")}</div>
            <p className="mt-1 text-sm text-muted-foreground">{t("Refresh the page to load the new version.")}</p>
          </div>
          <Button onClick={() => window.location.reload()}>{t("Refresh page")}</Button>
        </div>
      );
    }
    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 p-6 text-center">
        <TriangleAlertIcon className="size-10 text-amber-500" />
        <div>
          <div className="font-medium">{t("Something went wrong on this page")}</div>
          <p className="mt-1 max-w-md text-sm break-all text-muted-foreground">{error.message || String(error)}</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={() => this.setState({ error: null })}>
            {t("Retry")}
          </Button>
          <Button onClick={() => window.location.reload()}>{t("Refresh page")}</Button>
        </div>
      </div>
    );
  }
}
