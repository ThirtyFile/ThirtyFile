import { useQuery } from "@tanstack/react-query";
import { BuildingIcon, DatabaseIcon, UserIcon, type LucideIcon } from "lucide-react";
import { api, type Drive, type DriveKind, type LinkMakers, type Me, type Role } from "@/api";
import { keys } from "@/api/queryKeys";
import { t } from "@/lib/i18n";

export const ROLE_RANK: Record<Role, number> = { viewer: 0, contributor: 1, editor: 2, manager: 3, owner: 4 };

export const ROLE_LABEL: Record<Role, string> = {
  viewer: t("Viewer"),
  contributor: t("Contributor"),
  editor: t("Editor"),
  manager: t("Manager"),
  owner: t("Owner"),
};

export const ROLE_HINT: Record<Role, string> = {
  viewer: t("Can browse, preview, and download"),
  contributor: t("Can add files and folders, and change or delete only what they added"),
  editor: t("Can upload, edit, delete, and create share links, as far as their account and the space allow"),
  manager: t("Editor permissions, plus manage members and sharing"),
  owner: t("Manager permissions, plus delete the space"),
};

export function atLeast(role: Role | null | undefined, min: Role) {
  return !!role && ROLE_RANK[role] >= ROLE_RANK[min];
}

/** Who may make share links of a space's items, as the space's Members dialog offers them */
export const LINK_MAKERS_LABEL: Record<LinkMakers, string> = {
  editors: t("Editors, managers and owners"),
  managers: t("Managers and owners"),
  nobody: t("Nobody"),
};

/** Whether a role may make share links where the space lets `makers` make them (the backend's LinkMakers::allow) */
export function linkAllowed(role: Role | null | undefined, makers: LinkMakers = "editors") {
  return makers === "editors" ? atLeast(role, "editor") : makers === "managers" && atLeast(role, "manager");
}

/** Effective capabilities = role ∩ account permissions (matches the backend's tree::allows and tree::may_link). A
 * folder space (`readOnly`) can be browsed, downloaded and shared, but not changed from the web yet; `linkMakers`:
 * whom the space lets make share links. `add`: upload, create, paste or restore into the folder; `write` and `del`:
 * change and delete what is in it, and for a contributor (`ownWrite`, `ownDel`) only what they added (`isMine`). */
export function capsOf(role: Role | null | undefined, me: Me, readOnly = false, linkMakers?: LinkMakers) {
  const admin = me.role === "admin";
  const contributor = !readOnly && role === "contributor";
  return {
    add: !readOnly && atLeast(role, "contributor") && (me.can_write || admin),
    write: !readOnly && atLeast(role, "editor") && (me.can_write || admin),
    del: !readOnly && atLeast(role, "editor") && (me.can_delete || admin),
    ownWrite: contributor && (me.can_write || admin),
    ownDel: contributor && (me.can_delete || admin),
    share: linkAllowed(role, linkMakers) && (me.can_share || admin),
    manage: atLeast(role, "manager"),
  };
}

/** Whether I added the item myself (the backend's tree::is_own): an item found on a folder space's disk has no owner name */
export function isMine(item: { owner_name: string }, me: Me) {
  return !!item.owner_name && item.owner_name === me.username;
}

/** `capsOf` for these items: a contributor changes and deletes them only when they are all theirs */
export function capsFor<C extends ReturnType<typeof capsOf>>(caps: C, items: readonly { owner_name: string }[], me: Me): C {
  const mine = items.length > 0 && items.every((n) => isMine(n, me));
  return { ...caps, write: caps.write || (caps.ownWrite && mine), del: caps.del || (caps.ownDel && mine) };
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
