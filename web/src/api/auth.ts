import { request, get, post, enc, qs } from "@/api/client";
import type { SsoProvider, LinkedIdentity, Me, TwoFactorPending, TwoFactorSetup, TwoFactorStatus, Device, AppPassword } from "@/api/types";

/** Signing in, two-factor sign-in, devices, app passwords and linked sign-in methods */
export const authApi = {
  me: () => get<Me>("/auth/me"),
  login: (username: string, password: string) => post<Me | TwoFactorPending>("/auth/login", { username, password }),
  /** The second step of signing in; after setting it up, the answer carries the recovery codes */
  loginCode: (ticket: string, code: string) => post<Me & { recovery_codes?: string[] }>("/auth/login/2fa", { ticket, code }),
  loginSetup: (ticket: string) => post<TwoFactorSetup>("/auth/login/2fa/setup", { ticket }),
  twoFactor: () => get<TwoFactorStatus>("/auth/2fa"),
  /** Once two-factor sign-in is on, changing it also takes a code from the app or a recovery code */
  startTwoFactor: (password: string, code?: string) => post<TwoFactorSetup>("/auth/2fa/setup", { password, code }),
  enableTwoFactor: (code: string) => post<{ recovery_codes: string[] }>("/auth/2fa/enable", { code }),
  disableTwoFactor: (password: string, code?: string) => post("/auth/2fa/disable", { password, code }),
  newRecoveryCodes: (password: string, code?: string) => post<{ recovery_codes: string[] }>("/auth/2fa/recovery-codes", { password, code }),
  resetTwoFactor: (userId: number) => request("DELETE", `/admin/users/${userId}/2fa`),
  logout: () => post("/auth/logout"),
  changePassword: (current: string, next: string) => request("PUT", "/auth/password", { current, new: next }),
  authOptions: () => get<{ password_reset: boolean }>("/auth/options"),
  forgotPassword: (account: string) => post("/auth/forgot", { account }),
  resetPassword: (token: string, next: string) => post("/auth/reset", { token, new: next }),
  devices: () => get<Device[]>("/auth/sessions"),
  signOutDevice: (id: string) => request("DELETE", `/auth/sessions/${encodeURIComponent(id)}`),
  signOutOtherDevices: () => post<{ removed: number }>("/auth/sessions/others"),
  userDevices: (userId: number) => get<Device[]>(`/admin/users/${userId}/sessions`),
  signOutUserDevice: (userId: number, id: string) => request("DELETE", `/admin/users/${userId}/sessions/${encodeURIComponent(id)}`),
  signOutUserDevices: (userId: number) => request<{ removed: number }>("DELETE", `/admin/users/${userId}/sessions`),
  appPasswords: () => get<AppPassword[]>("/auth/app-passwords"),
  createAppPassword: (req: { name: string; scope: "read" | "write"; expires_days?: number; password?: string; code?: string }) =>
    post<{ token: string; app_password: AppPassword }>("/auth/app-passwords", req),
  deleteAppPassword: (id: string) => request("DELETE", `/auth/app-passwords/${encodeURIComponent(id)}`),
  ssoProviders: () => get<SsoProvider[]>("/auth/sso/providers"),
  /** Start a third-party login (full-page redirect); link = link to the currently signed-in account */
  ssoStartUrl: (provider: string, next: string) => enc`/api/auth/sso/${provider}/start` + qs({ next }),
  /** Linking starts with a request from this page, which returns where to go next */
  /** Linking takes the current password (and a two-factor code); accounts without a password, a recent sign-in */
  ssoLink: (provider: string, next: string, password?: string, code?: string) => post<{ url: string }>(enc`/auth/sso/${provider}/link`, { next, password, code }),
  myIdentities: () => get<{ linked: LinkedIdentity[]; available: string[] }>("/auth/identities"),
  unlinkIdentity: (provider: string) => request("DELETE", enc`/auth/identities/${provider}`),
};
