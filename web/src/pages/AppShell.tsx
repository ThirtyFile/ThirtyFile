import { Suspense, useEffect, useLayoutEffect } from "react";
import { Loader2Icon } from "lucide-react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { toast } from "sonner";
import { SSO_LABEL, type SsoProviderId } from "@/components/ProviderIcon";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { useQueryClient } from "@tanstack/react-query";
import { TabBar } from "@/components/TabBar";
import { NotificationBell } from "@/components/NotificationBell";
import { UploadPanel } from "@/components/UploadPanel";
import { DownloadPanel } from "@/components/DownloadPanel";
import { useMe } from "@/lib/session";
import { MAIN_ID } from "@/components/Frame";
import { loadTabs, syncLocation } from "@/tabs";
import { loadTree } from "@/components/FolderTree";
import { onUploadsLanded } from "@/uploads";
import { api, type SortKey, type SortOrder } from "@/api";
import { refreshFirstPage } from "@/lib/pages";
import { refreshFiles } from "@/lib/queries";
import { t, tServer } from "@/lib/i18n";
import { takeSsoError } from "@/lib/signInReturn";

export function AppShell() {
  const me = useMe();
  const qc = useQueryClient();
  const location = useLocation();
  const navigate = useNavigate();
  // Before the first paint, so the tab bar and the navigation pane show this user's tabs and folders right away
  useLayoutEffect(() => {
    loadTabs(me.id);
    loadTree(me.id);
  }, [me.id]);

  useEffect(() => {
    const params = new URLSearchParams(location.search);
    const linked = params.get("sso_linked");
    const error = takeSsoError(params);
    if (!linked && !error) return;
    if (linked) toast.success(t("Your {provider} account is linked. You can use it to sign in from now on.", { provider: SSO_LABEL[linked as SsoProviderId] ?? linked }));
    if (error) toast.error(tServer(error));
    params.delete("sso_linked");
    params.delete("sso_error");
    const rest = params.toString();
    navigate(location.pathname + (rest ? `?${rest}` : ""), { replace: true });
  }, [location.search, location.pathname, navigate]);

  // Record URL changes in the current tab's history
  useEffect(() => {
    syncLocation(location.pathname + location.search);
  }, [location.pathname, location.search]);

  // As uploaded files land, refresh the first page of the folders they went to (not every open folder, nor the rest of
  // a big folder), at most every 1.5 s; when the uploads end, once more the lists of the folders they went to (and,
  // for uploaded folders, the lists already loaded below them), and the used space. A refresh under way then starts
  // again, so it can't miss the last files.
  useEffect(
    () =>
      onUploadsLanded((parentIds, final, batch) => {
        if (final) {
          void refreshFiles(qc, { folders: batch.folders, trees: batch.trees, contents: true, usage: true, recent: true, trash: true });
          return;
        }
        // Folder lists loaded page by page are ["children", id, sort, order]; a folder loaded a part at a time
        // (lib/windows) loads the parts in view again (the others once they show). The folder tree's waits for the end.
        for (const id of parentIds) {
          void refreshFirstPage(qc, ["children", id], ([, , sort, order], limit) => api.childrenPage(id, sort as SortKey, order as SortOrder, limit));
          void qc.invalidateQueries({ queryKey: ["children", id], predicate: (q) => q.queryKey[4] === "at" });
        }
      }),
    [qc],
  );

  return (
    <div className="flex h-full flex-col">
      {/* First stop for Tab: past the tab bar, address bar, command bar and navigation pane */}
      <a
        href={`#${MAIN_ID}`}
        onClick={(e) => {
          const main = document.getElementById(MAIN_ID);
          if (!main) return;
          e.preventDefault();
          main.focus();
        }}
        className="sr-only rounded-md bg-background px-3 py-2 text-sm font-medium shadow-lg ring-2 ring-ring focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-50"
      >
        {t("Skip to main content")}
      </a>
      <div className="flex shrink-0 bg-sidebar">
        <div className="min-w-0 flex-1">
          <ErrorBoundary>
            <TabBar />
          </ErrorBoundary>
        </div>
        <ErrorBoundary>
          <NotificationBell />
        </ErrorBoundary>
      </div>
      <div className="min-h-0 flex-1">
        {/* An error in one page only affects the content area; the tab bar and upload panel keep working */}
        <ErrorBoundary resetKey={location.pathname}>
          {/* Pages load when first opened: the tab bar and panels stay while one does */}
          <Suspense
            fallback={
              <div className="flex h-full items-center justify-center text-muted-foreground" role="status" aria-label={t("Loading…")}>
                <Loader2Icon className="size-6 animate-spin" />
              </div>
            }
          >
            <Outlet />
          </Suspense>
        </ErrorBoundary>
      </div>
      {/* Transfer progress at the bottom right: downloads above, uploads below (above the phone selection bar while it shows) */}
      <div className="fixed right-4 bottom-[calc(var(--tf-bottom-inset,0px)+1rem)] z-40 flex w-[min(380px,calc(100vw-2rem))] flex-col gap-2">
        <DownloadPanel />
        <UploadPanel />
      </div>
    </div>
  );
}
