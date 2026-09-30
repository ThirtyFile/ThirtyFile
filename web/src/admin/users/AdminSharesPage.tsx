import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link2OffIcon, type LucideIcon } from "lucide-react";
import { driveName } from "@/api";
import { queries } from "@/api/queryKeys";
import { EmptyState } from "@/components/DataTable";
import { ToolSeparator } from "@/components/Frame";
import { controlPanelItem, useSettingsSearch } from "@/admin/controlPanel";
import { t } from "@/lib/i18n";
import { ShareLinks } from "@/components/ShareLinks";
import { NativeSelect } from "@/components/ui/native-select";


/** Control panel › All share links: every public link, filtered by space, creator and state, to find and revoke them */
export function AdminSharesPage() {
  const { title, icon } = controlPanelItem("shares");
  const searchSettings = useSettingsSearch();
  const drives = useQuery(queries.adminDrives);
  const users = useQuery(queries.adminUsers);
  const [driveId, setDriveId] = useState("");
  const [ownerId, setOwnerId] = useState("");
  const [state, setState] = useState<"" | "active" | "expired">("");

  return (
    <ShareLinks
      filter={{
        scope: "managed",
        drive_id: driveId || undefined,
        owner_id: ownerId ? Number(ownerId) : undefined,
        expired: state ? state === "expired" : undefined,
      }}
      frame={{
        crumbs: [{ label: t("Control panel"), to: "/admin" }, { label: title }],
        icon: icon as LucideIcon,
        upTo: "/admin",
        searchPlaceholder: t("Search settings"),
        onSearch: searchSettings,
      }}
      showOwner
      extraToolbar={
        <>
          <ToolSeparator />
          <NativeSelect size="xs" className="max-w-44" aria-label={t("Space")} value={driveId} onChange={(e) => setDriveId(e.target.value)}>
            <option value="">{t("All spaces")}</option>
            {(drives.data ?? []).map((d) => (
              <option key={d.id} value={d.id}>
                {d.kind === "personal" ? t("My files of {name}", { name: d.owner_name }) : driveName(d)}
              </option>
            ))}
          </NativeSelect>
          <NativeSelect size="xs" className="max-w-44" aria-label={t("Created by")} value={ownerId} onChange={(e) => setOwnerId(e.target.value)}>
            <option value="">{t("Everyone")}</option>
            {(users.data ?? []).map((u) => (
              <option key={u.id} value={u.id}>
                {u.username}
              </option>
            ))}
          </NativeSelect>
          <NativeSelect size="xs" className="max-w-44" aria-label={t("State")} value={state} onChange={(e) => setState(e.target.value as typeof state)}>
            <option value="">{t("All links")}</option>
            <option value="active">{t("Working")}</option>
            <option value="expired">{t("Expired or used up")}</option>
          </NativeSelect>
        </>
      }
      empty={<EmptyState icon={Link2OffIcon} title={t("No share links")} hint={t("No public link matches these filters.")} />}
    />
  );
}
