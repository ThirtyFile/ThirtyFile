import type { Branding } from "@/lib/branding";
import { request, get, post, enc } from "@/api/client";
import type { BrandingReq, DashboardInfo, EmailSettings, EmailSettingsReq, MonitoringReq, MonitoringSettings, SsoSettings, SsoSettingsReq, SystemSettingsReq, SystemInfo } from "@/api/types";

/** Who is saving, asked again (see `useConfirmIdentity`) */
type Confirmation = { my_password?: string; my_code?: string };

/** System settings, branding, email and single sign-on (administrators) */
export const settingsApi = {
  systemSettings: () => get<SystemInfo>("/admin/settings"),
  updateSystemSettings: (req: SystemSettingsReq) => request<SystemInfo>("PATCH", "/admin/settings", req),
  branding: () => get<Branding>("/branding"),
  updateBranding: (b: BrandingReq) => request<Branding>("PUT", "/admin/branding", b),
  uploadLogo: (variant: "light" | "dark", file: File) => request<Branding>("PUT", enc`/admin/branding/logo/${variant}`, undefined, file),
  deleteLogo: (variant: "light" | "dark") => request<Branding>("DELETE", enc`/admin/branding/logo/${variant}`),
  uploadLoginBackground: (file: File) => request<Branding>("PUT", "/admin/branding/background", undefined, file),
  deleteLoginBackground: () => request<Branding>("DELETE", "/admin/branding/background"),
  emailSettings: () => get<EmailSettings>("/admin/email"),
  /** Saving takes the administrator's own password (and code): `my_password`, `my_code` */
  updateEmailSettings: (req: EmailSettingsReq & Confirmation) => request<EmailSettings>("PUT", "/admin/email", req),
  testEmail: (req: EmailSettingsReq & { to: string }) => post("/admin/email/test", req),
  ssoSettings: () => get<SsoSettings>("/admin/sso"),
  monitoring: () => get<MonitoringSettings>("/admin/monitoring"),
  dashboard: (span: number, signal?: AbortSignal) => get<DashboardInfo>(`/admin/dashboard?span=${span}`, signal),
  saveMonitoring: (s: MonitoringReq & Confirmation) => request<MonitoringSettings & { token?: string }>("PUT", "/admin/monitoring", s),
  /** Saving takes the administrator's own password (and code), like the email settings */
  updateSsoSettings: (s: SsoSettingsReq & Confirmation) => request<SsoSettings>("PUT", "/admin/sso", s),
};
