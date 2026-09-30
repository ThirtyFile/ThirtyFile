//! The administration pages: one for each Control panel item (admin/controlPanel.ts), at /admin/<item>. App.tsx builds
//! their routes from this table; each page loads when an administrator first opens it.

import type { ComponentType } from "react";
import type { ControlPanelKey } from "@/admin/controlPanel";

export const ADMIN_PAGES: Record<ControlPanelKey, () => Promise<ComponentType>> = {
  users: () => import("@/admin/users/AdminUsersPage").then((m) => m.AdminUsersPage),
  groups: () => import("@/admin/users/GroupsPage").then((m) => m.GroupsPage),
  shares: () => import("@/admin/users/AdminSharesPage").then((m) => m.AdminSharesPage),
  sso: () => import("@/admin/users/SsoPage").then((m) => m.SsoPage),
  drives: () => import("@/admin/storage/AdminDrivesPage").then((m) => m.AdminDrivesPage),
  storage: () => import("@/admin/storage/StorageSettingsPage").then((m) => m.StorageSettingsPage),
  moves: () => import("@/admin/storage/MovesPage").then((m) => m.MovesPage),
  backups: () => import("@/admin/storage/BackupsPage").then((m) => m.BackupsPage),
  replicas: () => import("@/admin/storage/ReplicasPage").then((m) => m.ReplicasPage),
  usage: () => import("@/admin/storage/UsageSettingsPage").then((m) => m.UsageSettingsPage),
  general: () => import("@/admin/system/GeneralSettingsPage").then((m) => m.GeneralSettingsPage),
  branding: () => import("@/admin/system/BrandingPage").then((m) => m.BrandingPage),
  email: () => import("@/admin/system/EmailPage").then((m) => m.EmailPage),
  activity: () => import("@/admin/system/ActivitySettingsPage").then((m) => m.ActivitySettingsPage),
  logs: () => import("@/admin/system/LogSettingsPage").then((m) => m.LogSettingsPage),
};

/** A Control panel item's address */
export const adminPath = (key: ControlPanelKey) => `/admin/${key}`;
