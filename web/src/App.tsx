import { Suspense, lazy, useEffect } from "react";
import { noteSignedIn } from "@/lib/signOut";
import { Navigate, Route, Routes, useLocation, useNavigate } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { ApiError } from "@/api";
import { keys, queries } from "@/api/queryKeys";
import { Button } from "@/components/ui/button";
import { MeContext } from "@/lib/session";
import { StyleChoiceContext, styleChoice } from "@/lib/style";
import { adoptLanguage, languageToAdopt, t } from "@/lib/i18n";
import { errorMessage, unreachable } from "@/lib/utils";
import { useApplyBranding } from "@/lib/branding";
import { ConfirmHost } from "@/components/confirm";
import type { ControlPanelKey } from "@/admin/controlPanel";
import { ADMIN_PAGES, adminPath } from "@/admin/pages";

// Each part loads when it is first needed: the sign-in page and a share link don't load the file explorer, and the
// administration pages load only when an administrator opens them
const page = <M, K extends keyof M, P = object>(load: () => Promise<M>, name: K) => lazy(() => load().then((m) => ({ default: m[name] as React.ComponentType<P> })));
const LoginPage = page(() => import("@/pages/LoginPage"), "LoginPage");
const ResetPasswordPage = page(() => import("@/pages/ResetPasswordPage"), "ResetPasswordPage");
const PublicSharePage = page(() => import("@/pages/PublicSharePage"), "PublicSharePage");
const AppShell = page(() => import("@/pages/AppShell"), "AppShell");
const ConflictHost = page(() => import("@/components/ConflictDialog"), "ConflictHost");
const TagDialogHost = page(() => import("@/components/tags"), "TagDialogHost");
const SmartFolderDialogHost = page(() => import("@/components/smartFolders"), "SmartFolderDialogHost");
const ChangePasswordDialog = page<typeof import("@/components/ChangePasswordDialog"), "ChangePasswordDialog", { required?: boolean; onClose(): void }>(
  () => import("@/components/ChangePasswordDialog"),
  "ChangePasswordDialog",
);
const FilesPage = page(() => import("@/pages/FilesPage"), "FilesPage");
const FileViewPage = page(() => import("@/pages/FileViewPage"), "FileViewPage");
const RecentPage = page(() => import("@/pages/ListPages"), "RecentPage");
const FavoritesPage = page(() => import("@/pages/ListPages"), "FavoritesPage");
const TaggedPage = page(() => import("@/pages/ListPages"), "TaggedPage");
const SmartFolderPage = page(() => import("@/pages/SmartFolderPage"), "SmartFolderPage");
const SearchPage = page(() => import("@/pages/ListPages"), "SearchPage");
const ThisPcPage = page(() => import("@/pages/ThisPcPage"), "ThisPcPage");
const SharedWithMePage = page(() => import("@/pages/SharedWithMePage"), "SharedWithMePage");
const SharesPage = page(() => import("@/pages/SharesPage"), "SharesPage");
const TrashPage = page(() => import("@/pages/TrashPage"), "TrashPage");
const ControlPanelPage = page(() => import("@/admin/ControlPanelPage"), "ControlPanelPage");
/** The administration pages, at their Control panel item's address */
const ADMIN_ROUTES = (Object.keys(ADMIN_PAGES) as ControlPanelKey[]).map((key) => ({
  path: adminPath(key),
  Page: lazy(() => ADMIN_PAGES[key]().then((Page) => ({ default: Page }))),
}));

function Spinner() {
  return (
    <div className="flex h-full items-center justify-center text-muted-foreground">
      <Loader2Icon className="size-6 animate-spin" />
    </div>
  );
}

function RequireAuth() {
  const me = useQuery(queries.me);
  const location = useLocation();
  const navigate = useNavigate();
  const qc = useQueryClient();

  // Someone else signing in in this tab after a session expired gets a fresh page
  useEffect(() => {
    if (me.data) noteSignedIn(me.data.id);
  }, [me.data]);

  // Signed in on a page in another language than theirs (the sign-in page had the browser's): reload in theirs
  const adopting = languageToAdopt(me.data?.ui_lang);
  useEffect(() => {
    if (adopting) adoptLanguage(adopting);
  }, [adopting]);

  // Any API returning 401 (session expired) sends the user back to the login page
  useEffect(() => {
    const onUnauthorized = () => {
      qc.clear();
      navigate(`/login?next=${encodeURIComponent(location.pathname + location.search)}`);
    };
    window.addEventListener("tf:unauthorized", onUnauthorized);
    return () => window.removeEventListener("tf:unauthorized", onUnauthorized);
  }, [navigate, location, qc]);

  if (me.isLoading || adopting)
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  if (me.error instanceof ApiError && me.error.status === 401) return <Navigate to={`/login?next=${encodeURIComponent(location.pathname + location.search)}`} replace />;
  if (!me.data)
    return (
      <div className="flex flex-col items-center gap-3 p-10 text-center">
        <p className="text-destructive">{errorMessage(me.error, unreachable())}</p>
        <Button variant="outline" size="sm" disabled={me.isFetching} onClick={() => void me.refetch()}>
          {me.isFetching && <Loader2Icon className="animate-spin" />}
          {t("Retry")}
        </Button>
      </div>
    );

  // A password an administrator chose is replaced first; the server allows nothing else meanwhile
  if (me.data.must_change_password)
    return (
      <MeContext.Provider value={me.data}>
        <ChangePasswordDialog required onClose={() => void qc.invalidateQueries({ queryKey: keys.me() })} />
      </MeContext.Provider>
    );

  return (
    <MeContext.Provider value={me.data}>
      <StyleChoiceContext.Provider value={styleChoice(me.data.style)}>
        <AppShell />
        {/* "Replace or skip" questions before uploading, moving, copying or restoring: they read the signed-in user's settings */}
        <ConflictHost />
        {/* Making and changing tags, from the menus, the details pane and the navigation pane */}
        <TagDialogHost />
        {/* Saving a search as a smart folder, and changing one */}
        <SmartFolderDialogHost />
      </StyleChoiceContext.Provider>
    </MeContext.Provider>
  );
}

function AdminOnly({ children }: { children: React.ReactNode }) {
  const me = useQuery(queries.me);
  return me.data?.role === "admin" ? <Suspense fallback={<Spinner />}>{children}</Suspense> : <Navigate to="/files" replace />;
}

export function App() {
  useApplyBranding();
  return (
    <>
      {/* Questions asked with confirm() from outside components */}
      <ConfirmHost />
      <Suspense fallback={<Spinner />}>
        <Routes>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/reset-password" element={<ResetPasswordPage />} />
          <Route path="/share/:token/:nodeId?" element={<PublicSharePage />} />
          <Route element={<RequireAuth />}>
            <Route index element={<Navigate to="/files" replace />} />
            <Route path="/files/:id?" element={<FilesPage />} />
            <Route path="/view/:id" element={<FileViewPage />} />
            <Route path="/all" element={<Navigate to="/files/shared" replace />} />
            <Route path="/drives" element={<ThisPcPage />} />
            <Route path="/shared-with-me" element={<SharedWithMePage />} />
            <Route path="/recent" element={<RecentPage />} />
            <Route path="/favorites" element={<FavoritesPage />} />
            <Route path="/tags/:id" element={<TaggedPage />} />
            <Route path="/smart/:id" element={<SmartFolderPage />} />
            <Route path="/search" element={<SearchPage />} />
            <Route path="/shares" element={<SharesPage />} />
            <Route path="/trash" element={<TrashPage />} />
            <Route
              path="/admin"
              element={
                <AdminOnly>
                  <ControlPanelPage />
                </AdminOnly>
              }
            />
            {ADMIN_ROUTES.map(({ path, Page }) => (
              <Route
                key={path}
                path={path}
                element={
                  <AdminOnly>
                    <Page />
                  </AdminOnly>
                }
              />
            ))}
            {/* Old URL: system settings were merged into the control panel */}
            <Route path="/admin/system" element={<Navigate to="/admin" replace />} />
            <Route path="*" element={<Navigate to="/files" replace />} />
          </Route>
        </Routes>
      </Suspense>
    </>
  );
}
