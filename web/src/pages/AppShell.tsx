import { useEffect, useLayoutEffect } from "react";
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
import { cn } from "@/lib/utils";
import { logoUrl, useBranding } from "@/lib/branding";
import { SiteName } from "@/components/SiteName";
import { MAIN_ID } from "@/components/Frame";
import { loadTabs, syncLocation } from "@/tabs";
import { onUploadsLanded } from "@/uploads";
import { t, tServer } from "@/lib/i18n";

/** Site logo and name (from branding settings; switches automatically when there's a dark-mode logo) */
export function Logo({ className, imgClassName = "h-7" }: { className?: string; imgClassName?: string }) {
  const b = useBranding();
  const img = cn("w-auto max-w-48 object-contain", imgClassName);
  return (
    <div className={cn("flex items-center gap-2 font-semibold", className)}>
      {!b.has_logo ? (
        <img src="/favicon.svg" alt="" className={cn(img, "aspect-square")} />
      ) : b.has_logo_dark ? (
        <>
          <img src={logoUrl(b)} alt={b.show_name ? "" : b.site_name} className={cn(img, "dark:hidden")} />
          <img src={logoUrl(b, true)} alt={b.show_name ? "" : b.site_name} className={cn(img, "hidden dark:block")} />
        </>
      ) : (
        <img src={logoUrl(b)} alt={b.show_name ? "" : b.site_name} className={img} />
      )}
      {(b.show_name || !b.has_logo) && <SiteName name={b.site_name} />}
    </div>
  );
}

export function AppShell() {
  const me = useMe();
  const qc = useQueryClient();
  const location = useLocation();
  const navigate = useNavigate();
  // Before the first paint, so the tab bar shows this user's tabs right away
  useLayoutEffect(() => loadTabs(me.id), [me.id]);

  useEffect(() => {
    const params = new URLSearchParams(location.search);
    const linked = params.get("sso_linked");
    const error = params.get("sso_error");
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

  // As uploaded files land, refresh the folders they went to (not every open folder), at most every 1.5 s; when the
  // uploads end, once more every list (uploaded folders add subfolders) and the used space. A refresh already under
  // way is left to finish instead of being restarted.
  useEffect(
    () =>
      onUploadsLanded((parentIds, final) => {
        const keys = final ? [["children"], ["recent"], ["me"]] : parentIds.map((id) => ["children", id]);
        for (const queryKey of keys) qc.invalidateQueries({ queryKey }, { cancelRefetch: false });
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
          <Outlet />
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
