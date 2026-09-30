//! Where a folder or file is, as the address bar shows it

import { UsersRoundIcon } from "lucide-react";
import type { NodeInfo } from "@/api";
import type { Crumb } from "@/components/Frame";
import { DRIVE_ICON } from "@/lib/drives";
import { t } from "@/lib/i18n";

/** Build the address bar from node info: All spaces › space › folder…, or Shared with me › shared folder… */
export function locationOf(info: NodeInfo | undefined) {
  if (!info) return { crumbs: [{ label: t("All spaces"), to: "/drives", virtual: true }] as Crumb[], rootUrl: "/drives", rootLabel: "", icon: undefined };
  const via = info.via_share;
  const rootLabel = via ? t("Shared with me") : info.drive.name;
  const rootUrl = via ? "/shared-with-me" : `/files/${info.drive.root_id}`;
  const crumbs: Crumb[] = [
    ...(via ? [] : [{ label: t("All spaces"), to: "/drives", virtual: true }]),
    { label: rootLabel, to: rootUrl },
    ...info.path.map((c) => ({ label: c.name, to: `/files/${c.id}` })),
  ];
  return { crumbs, rootUrl, rootLabel, icon: via ? UsersRoundIcon : DRIVE_ICON[info.drive.kind] };
}
