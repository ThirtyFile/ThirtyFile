import { useQuery } from "@tanstack/react-query";
import { BuildingIcon, DatabaseIcon, UserIcon, type LucideIcon } from "lucide-react";
import { api, type Drive, type DriveKind, type Me, type Role } from "@/api";
import { t } from "@/lib/i18n";

export const ROLE_RANK: Record<Role, number> = { viewer: 0, editor: 1, manager: 2, owner: 3 };

export const ROLE_LABEL: Record<Role, string> = {
  viewer: t("Viewer"),
  editor: t("Editor"),
  manager: t("Manager"),
  owner: t("Owner"),
};

export const ROLE_HINT: Record<Role, string> = {
  viewer: t("Can browse, preview, and download"),
  editor: t("Can upload, edit, delete, and create share links, as far as their account allows"),
  manager: t("Editor permissions, plus manage members and sharing"),
  owner: t("Manager permissions, plus delete the space"),
};

export function atLeast(role: Role | null | undefined, min: Role) {
  return !!role && ROLE_RANK[role] >= ROLE_RANK[min];
}

/** Effective capabilities = role ∩ account permissions (matches the backend's tree::allows). A folder space
 * (`readOnly`) can be browsed, downloaded and shared, but not changed from the web yet */
export function capsOf(role: Role | null | undefined, me: Me, readOnly = false) {
  const admin = me.role === "admin";
  return {
    write: !readOnly && atLeast(role, "editor") && (me.can_write || admin),
    del: !readOnly && atLeast(role, "editor") && (me.can_delete || admin),
    share: atLeast(role, "editor") && (me.can_share || admin),
    manage: atLeast(role, "manager"),
  };
}

export const DRIVE_ICON: Record<DriveKind, LucideIcon> = {
  personal: UserIcon,
  company: BuildingIcon,
  team: DatabaseIcon,
};

export const DRIVE_KIND_LABEL: Record<DriveKind, string> = {
  personal: t("Personal space"),
  company: t("Company shared space"),
  team: t("Team space"),
};

/** Spaces I can access (cached, shared by the left-hand menu, the folder picker dialog, etc.) */
export function useDrives() {
  // Refresh periodically: the view follows when a storage service goes offline / recovers
  return useQuery({ queryKey: ["drives"], queryFn: api.drives, staleTime: 30_000, refetchInterval: 30_000 });
}

export function driveLabel(d: Pick<Drive, "kind" | "name" | "owner_name">, forAdmin = false) {
  return d.kind === "personal" && forAdmin ? t("{name}'s files", { name: d.owner_name }) : d.name;
}

const ACTION_LABEL: Record<string, string> = {
  upload: t("Upload"),
  create_folder: t("Create folder"),
  rename: t("Rename"),
  move: t("Move"),
  copy: t("Copy"),
  trash: t("Move to trash"),
  restore: t("Restore"),
  delete: t("Delete permanently"),
  empty_trash: t("Empty trash"),
  edit: t("Edit"),
  grant: t("Grant access"),
  revoke: t("Remove access"),
  drive_create: t("Create space"),
  scan: t("Checked the folder"),
  drive_update: t("Update space"),
  drive_delete: t("Delete space"),
  group_create: t("Create group"),
  group_update: t("Update group"),
  group_delete: t("Delete group"),
  user_create: t("Add user"),
  user_update: t("Modify user"),
  user_delete: t("Delete user"),
  share_create: t("Create share link"),
  share_delete: t("Disable share link"),
  drive_location: t("Change storage location"),
  storage_create: t("Add storage location"),
  storage_update: t("Modify storage location"),
  storage_delete: t("Delete storage location"),
  storage_default: t("Set default storage location"),
  settings: t("System settings"),
  log_archive: t("Archive logs"),
  log_archive_delete: t("Delete archive"),
};

/** Action categories for the activity log filter */
export const ACTION_GROUPS: { label: string; actions: string[] }[] = [
  { label: t("Files"), actions: ["upload", "create_folder", "edit", "rename", "move", "copy", "trash", "restore", "delete", "empty_trash"] },
  { label: t("Sharing and permissions"), actions: ["share_create", "share_delete", "grant", "revoke"] },
  { label: t("Spaces"), actions: ["drive_create", "drive_update", "drive_delete", "drive_location"] },
  { label: t("Users and groups"), actions: ["user_create", "user_update", "user_delete", "group_create", "group_update", "group_delete"] },
  {
    label: t("System"),
    actions: ["settings", "storage_create", "storage_update", "storage_delete", "storage_default", "log_archive", "log_archive_delete"],
  },
];

export function actionLabel(a: string) {
  return ACTION_LABEL[a] ?? a;
}
