//! An account's permissions, named the same in the users list, the user dialog and the single sign-on rules

import { t } from "@/lib/i18n";

export const PERMISSIONS = ["can_write", "can_delete", "can_share"] as const;
export type Permission = (typeof PERMISSIONS)[number];

export const PERMISSION_LABEL: Record<Permission, string> = {
  can_write: t("Edit"),
  can_delete: t("Delete"),
  // Not only share links: the server checks the same permission for giving people access and managing members
  can_share: t("Share"),
};

/** What the labels cover, shown under them */
export const PERMISSIONS_HINT = t("Edit includes uploading. Share includes share links, Share with… and managing the members of a space.");
