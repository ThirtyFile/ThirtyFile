import type { Branding } from "@/lib/branding";
import { request, get, post, enc } from "@/api/client";
import type { BrandingReq, EmailSettings, EmailSettingsReq, SsoSettings, SsoSettingsReq, SystemSettingsReq, SystemInfo } from "@/api/types";

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
  updateEmailSettings: (req: EmailSettingsReq) => request<EmailSettings>("PUT", "/admin/email", req),
  testEmail: (req: EmailSettingsReq & { to: string }) => post("/admin/email/test", req),
  ssoSettings: () => get<SsoSettings>("/admin/sso"),
  updateSsoSettings: (s: SsoSettingsReq) => request<SsoSettings>("PUT", "/admin/sso", s),
};
