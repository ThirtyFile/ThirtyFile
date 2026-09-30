import { useQuery } from "@tanstack/react-query";
import { BuildingIcon, DatabaseIcon, UserIcon, type LucideIcon } from "lucide-react";
import { api, type Drive, type DriveKind, type Me, type Role } from "@/api";
import { keys } from "@/api/queryKeys";
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
  return useQuery({ queryKey: keys.drives(), queryFn: api.drives, staleTime: 30_000, refetchInterval: 30_000 });
}

export function driveLabel(d: Pick<Drive, "kind" | "name" | "owner_name">, forAdmin = false) {
  return d.kind === "personal" && forAdmin ? t("{name}'s files", { name: d.owner_name }) : d.name;
}
