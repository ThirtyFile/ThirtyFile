import { Suspense, lazy, useEffect } from "react";
import { noteSignedIn } from "@/lib/signOut";
import { Navigate, Route, Routes, useLocation, useNavigate } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { api, ApiError } from "@/api";
import { Button } from "@/components/ui/button";
import { MeContext } from "@/lib/session";
import { t } from "@/lib/i18n";
import { useApplyBranding } from "@/lib/branding";
import { AppShell } from "@/pages/AppShell";
import { FilesPage } from "@/pages/FilesPage";
import { FileViewPage } from "@/pages/FileViewPage";
import { FavoritesPage, RecentPage, SearchPage } from "@/pages/ListPages";
import { ThisPcPage } from "@/pages/ThisPcPage";
import { SharedWithMePage } from "@/pages/SharedWithMePage";
import { LoginPage } from "@/pages/LoginPage";
import { ResetPasswordPage } from "@/pages/ResetPasswordPage";
import { ChangePasswordDialog } from "@/components/dialogs";
import { PublicSharePage } from "@/pages/PublicSharePage";
import { SharesPage } from "@/pages/SharesPage";
import { TrashPage } from "@/pages/TrashPage";
import { ConfirmHost } from "@/components/confirm";
import { ConflictHost } from "@/components/ConflictDialog";

// Administration pages: loaded only when an administrator opens them
const page = <M, K extends keyof M>(load: () => Promise<M>, name: K) =>
  lazy(() => load().then((m) => ({ default: m[name] as React.ComponentType })));
const ControlPanelPage = page(() => import("@/pages/ControlPanelPage"), "ControlPanelPage");
const AdminUsersPage = page(() => import("@/pages/AdminUsersPage"), "AdminUsersPage");
const GroupsPage = page(() => import("@/pages/GroupsPage"), "GroupsPage");
const AdminSharesPage = page(() => import("@/pages/AdminSharesPage"), "AdminSharesPage");
const AdminDrivesPage = page(() => import("@/pages/AdminDrivesPage"), "AdminDrivesPage");
const GeneralSettingsPage = page(() => import("@/pages/GeneralSettingsPage"), "GeneralSettingsPage");
const StorageSettingsPage = page(() => import("@/pages/StorageSettingsPage"), "StorageSettingsPage");
const UsageSettingsPage = page(() => import("@/pages/UsageSettingsPage"), "UsageSettingsPage");
const ActivitySettingsPage = page(() => import("@/pages/ActivitySettingsPage"), "ActivitySettingsPage");
const LogSettingsPage = page(() => import("@/pages/LogSettingsPage"), "LogSettingsPage");
const SsoPage = page(() => import("@/pages/SsoPage"), "SsoPage");
const EmailPage = page(() => import("@/pages/EmailPage"), "EmailPage");
const BrandingPage = page(() => import("@/pages/BrandingPage"), "BrandingPage");

function Spinner() {
  return (
    <div className="flex h-full items-center justify-center text-muted-foreground">
      <Loader2Icon className="size-6 animate-spin" />
    </div>
  );
}

function RequireAuth() {
  const me = useQuery({
    queryKey: ["me"],
    queryFn: api.me,
    retry: false,
    staleTime: 30_000,
  });
  const location = useLocation();
  const navigate = useNavigate();
  const qc = useQueryClient();

  // Someone else signing in in this tab after a session expired gets a fresh page
  useEffect(() => {
    if (me.data) noteSignedIn(me.data.id);
  }, [me.data]);

  // Any API returning 401 (session expired) sends the user back to the login page
  useEffect(() => {
    const onUnauthorized = () => {
      qc.clear();
      navigate(`/login?next=${encodeURIComponent(location.pathname + location.search)}`);
    };
    window.addEventListener("tf:unauthorized", onUnauthorized);
    return () => window.removeEventListener("tf:unauthorized", onUnauthorized);
  }, [navigate, location, qc]);

  if (me.isLoading)
    return (
      <div className="flex h-full items-center justify-center text-muted-foreground">
        <Loader2Icon className="size-6 animate-spin" />
      </div>
    );
  if (me.error instanceof ApiError && me.error.status === 401)
    return <Navigate to={`/login?next=${encodeURIComponent(location.pathname + location.search)}`} replace />;
  if (!me.data)
    return (
      <div className="flex flex-col items-center gap-3 p-10 text-center">
        <p className="text-destructive">{me.error instanceof ApiError ? me.error.message : t("Can't connect to the server")}</p>
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
        <ChangePasswordDialog required onClose={() => void qc.invalidateQueries({ queryKey: ["me"] })} />
      </MeContext.Provider>
    );

  return (
    <MeContext.Provider value={me.data}>
      <AppShell />
    </MeContext.Provider>
  );
}

function AdminOnly({ children }: { children: React.ReactNode }) {
  const me = useQuery({ queryKey: ["me"], queryFn: api.me });
  return me.data?.role === "admin" ? <Suspense fallback={<Spinner />}>{children}</Suspense> : <Navigate to="/files" replace />;
}

export function App() {
  useApplyBranding();
  return (
    <>
      {/* Questions asked with confirm() from outside components */}
      <ConfirmHost />
      {/* "Replace or skip" questions before uploading, moving, copying or restoring */}
      <ConflictHost />
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
          <Route
            path="/admin/users"
            element={
              <AdminOnly>
                <AdminUsersPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/shares"
            element={
              <AdminOnly>
                <AdminSharesPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/groups"
            element={
              <AdminOnly>
                <GroupsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/drives"
            element={
              <AdminOnly>
                <AdminDrivesPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/general"
            element={
              <AdminOnly>
                <GeneralSettingsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/storage"
            element={
              <AdminOnly>
                <StorageSettingsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/usage"
            element={
              <AdminOnly>
                <UsageSettingsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/activity"
            element={
              <AdminOnly>
                <ActivitySettingsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/logs"
            element={
              <AdminOnly>
                <LogSettingsPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/sso"
            element={
              <AdminOnly>
                <SsoPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/email"
            element={
              <AdminOnly>
                <EmailPage />
              </AdminOnly>
            }
          />
          <Route
            path="/admin/branding"
            element={
              <AdminOnly>
                <BrandingPage />
              </AdminOnly>
            }
          />
          {/* Old URL: system settings were merged into the control panel */}
          <Route path="/admin/system" element={<Navigate to="/admin" replace />} />
          <Route path="*" element={<Navigate to="/files" replace />} />
        </Route>
      </Routes>
    </>
  );
}
